import { useState, useEffect, useMemo } from 'react';
import { Plus, Search, Copy, Check, Trash2, Pencil, Shield, Gauge, Key as KeyIcon, Loader2, MoreVertical } from 'lucide-react';
import { Button } from '@/components/ui/button';
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog';
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu';
import { serviceKeysApi, providersApi, modelsApi, combosApi, installApi, type ServiceKey, type ServiceKeyUsage } from '@/lib/api';
import { useWebSocket } from '@/hooks/useWebSocket';
import { useT } from '@/i18n';
import { cn } from '@/lib/utils';

/**
 * 密钥页：管理本网关发放给客户端的 Service Key（不是上游供应商的密钥）。
 *
 * - 创建：网关生成明文密钥（仅此一次可见），附分发链接（LAN install 页 + key）。
 * - 权限：allowed_models 白名单（空 = 全部），按供应商分组 + 组合名分组。
 * - 额度：5h / 7d 滚动窗口 token 上限（0 = 不设限），列表实时显示已用量百分比
 *   与窗口剩余时间，触顶红色。
 * - 用量经 WS usage_stats_changed（后端每 5s 广播）实时刷新，60s 兜底轮询。
 */

type ServiceKeyRow = ServiceKey & ServiceKeyUsage;

/** 格式化时间戳为 YYYY-MM-DD HH:mm（本地时区）。 */
function formatTime(t: number): string {
  const d = new Date(t * 1000);
  const p = (n: number) => String(n).padStart(2, '0');
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())} ${p(d.getHours())}:${p(d.getMinutes())}`;
}

/** 滚窗剩余时间（秒 → XdYh / XhYm / Ym）。 */
function resetsIn(remainingSecs: number): string {
  const r = Math.max(0, Math.floor(remainingSecs));
  const d = Math.floor(r / 86400);
  const h = Math.floor((r % 86400) / 3600);
  const m = Math.max(1, Math.floor((r % 3600) / 60));
  if (d > 0) return `${d}d${h}h`;
  if (h > 0) return `${h}h${m}m`;
  return `${m}m`;
}

/** 省略读数：≥1e8 → 亿，≥1e4 → 万，0 → 不设限。 */
function formatAbbrev(n: number, t: (key: string) => string): string {
  if (!n || n <= 0) return t('keys.unlimited');
  if (n >= 1e8) return (n / 1e8).toFixed(2) + t('keys.unit_yi');
  if (n >= 1e4) return (n / 1e4).toFixed(2) + t('keys.unit_wan');
  return String(n);
}

/** 限额列窗口行：仅展示设了上限的窗口；used >= limit 视为触顶。 */
function quotaLines(k: ServiceKeyRow, t: (key: string) => string): { key: string; resets_in: string; percent: number; over: boolean }[] {
  const now = Math.floor(Date.now() / 1000);
  const lines: { key: string; resets_in: string; percent: number; over: boolean }[] = [];
  const push = (used: number, limit: number | undefined, label: string, windowSecs: number) => {
    if (!limit || limit <= 0) return;
    lines.push({
      key: label,
      resets_in: resetsIn(windowSecs - (now % windowSecs)),
      percent: (used / limit) * 100,
      over: used >= limit,
    });
  };
  push(k.used_5h ?? 0, k.quota_5h, '5h', 5 * 3600);
  push(k.used_7d ?? 0, k.quota_7d, '7d', 7 * 86400);
  return lines;
}

export function KeysView() {
  const t = useT();

  const [keys, setKeys] = useState<ServiceKeyRow[]>([]);
  const [loading, setLoading] = useState(true);
  const [searchQuery, setSearchQuery] = useState('');

  // Create
  const [createDialogOpen, setCreateDialogOpen] = useState(false);
  const [newName, setNewName] = useState('');
  const [createSet, setCreateSet] = useState<Set<string>>(new Set());
  const [creating, setCreating] = useState(false);
  const [createdKey, setCreatedKey] = useState<{ key: string; name: string } | null>(null);
  const [copied, setCopied] = useState(false);
  const [localIp, setLocalIp] = useState<string | null>(null);
  const [localPort, setLocalPort] = useState(19068);

  // Rename
  const [renameDialogOpen, setRenameDialogOpen] = useState(false);
  const [renameTarget, setRenameTarget] = useState<ServiceKeyRow | null>(null);
  const [renameName, setRenameName] = useState('');

  // Perm (allowed models)
  const [permDialogOpen, setPermDialogOpen] = useState(false);
  const [permTarget, setPermTarget] = useState<ServiceKeyRow | null>(null);
  const [permSet, setPermSet] = useState<Set<string>>(new Set());
  const [permGroups, setPermGroups] = useState<{ name: string; models: string[] }[]>([]);
  const [modelsLoading, setModelsLoading] = useState(false);

  // Quota
  const [quotaDialogOpen, setQuotaDialogOpen] = useState(false);
  const [quotaTarget, setQuotaTarget] = useState<ServiceKeyRow | null>(null);
  const [quota5h, setQuota5h] = useState('');
  const [quota7d, setQuota7d] = useState('');

  // Delete
  const [deleteDialogOpen, setDeleteDialogOpen] = useState(false);
  const [deleteTarget, setDeleteTarget] = useState<ServiceKeyRow | null>(null);
  const [saving, setSaving] = useState(false);

  /** 首次加载显示 spinner；后续刷新（WS / 轮询 / 增删改后）静默更新。 */
  async function fetchServiceKeys(silent = false) {
    if (!silent) setLoading(true);
    try {
      const list = await serviceKeysApi.list();
      setKeys(list);
    } finally {
      setLoading(false);
    }
  }

  // Live usage: WS（每 5s 广播）+ 60s 兜底轮询
  useWebSocket('usage_stats_changed', () => fetchServiceKeys(true));
  useEffect(() => {
    fetchServiceKeys();
    const timer = window.setInterval(() => fetchServiceKeys(true), 60000);
    return () => window.clearInterval(timer);
  }, []);

  // 分发链接：创建成功后拉取本机 LAN IP（取不到则仅展示明文）
  useEffect(() => {
    if (createdKey && !localIp) {
      installApi.localIp().then((r) => {
        if (r.ip) setLocalIp(r.ip);
        if (r.port) setLocalPort(r.port);
      });
    }
  }, [createdKey, localIp]);

  const deployLink = createdKey && localIp ? `http://${localIp}:${localPort}/install?t=${createdKey.key}` : '';

  // Filtered keys
  const filteredKeys = useMemo(() => {
    if (!searchQuery.trim()) return keys;
    const query = searchQuery.toLowerCase();
    return keys.filter(
      (k) => k.name.toLowerCase().includes(query) || k.key_masked.toLowerCase().includes(query)
    );
  }, [keys, searchQuery]);

  // ── Actions ────────────────────────────────────────────────────────────────

  const handleCreate = async () => {
    setCreating(true);
    try {
      const r = await serviceKeysApi.create({
        name: newName.trim() || t('common.unnamed'),
        allowed_models: [...createSet],
      });
      setCreatedKey({ key: r.key, name: r.name });
      setNewName('');
      await fetchServiceKeys(true);
    } catch (e: any) {
      console.error('Failed to create service key:', e);
    } finally {
      setCreating(false);
    }
  };

  const openCreate = () => {
    setNewName('');
    setCreateSet(new Set());
    setCreateDialogOpen(true);
    loadPermOptions();
  };

  const toggleCreatePerm = (model: string) => {
    setCreateSet((prev) => {
      const s = new Set(prev);
      s.has(model) ? s.delete(model) : s.add(model);
      return s;
    });
  };

  const handleRename = async () => {
    if (!renameTarget) return;
    setSaving(true);
    try {
      await serviceKeysApi.update(renameTarget.id, { name: renameName.trim() || t('common.unnamed') });
      setRenameDialogOpen(false);
      await fetchServiceKeys(true);
    } catch (e: any) {
      console.error('Failed to rename service key:', e);
    } finally {
      setSaving(false);
    }
  };

  const openPerms = (k: ServiceKeyRow) => {
    setPermTarget(k);
    setPermSet(new Set(k.allowed_models || []));
    setPermDialogOpen(true);
    loadPermOptions();
  };

  async function loadPermOptions() {
    setModelsLoading(true);
    try {
      const [providers, models, combos] = await Promise.all([
        providersApi.list(),
        modelsApi.list(),
        combosApi.list(),
      ]);
      const providerName = new Map(providers.map((p) => [p.id, p.name]));
      const groupsMap = new Map<string, string[]>();
      for (const m of models) {
        const pname = providerName.get(m.provider_id) || t('common.unknown');
        const name = m.display_name || m.model_id;
        if (!groupsMap.has(pname)) groupsMap.set(pname, []);
        if (!groupsMap.get(pname)!.includes(name)) groupsMap.get(pname)!.push(name);
      }
      const groups = Array.from(groupsMap.entries()).map(([name, ms]) => ({ name, models: ms.sort() }));
      // 组合别名独立分组：授予组合名 = 授予其全部成员
      const comboGroup = combos.filter((c) => c.enabled).map((c) => c.name).sort();
      if (comboGroup.length) {
        groups.push({ name: t('keys.perm_group_combos'), models: comboGroup });
      }
      setPermGroups(groups);
    } finally {
      setModelsLoading(false);
    }
  }

  const togglePerm = (model: string) => {
    setPermSet((prev) => {
      const s = new Set(prev);
      s.has(model) ? s.delete(model) : s.add(model);
      return s;
    });
  };

  const handleSavePerms = async () => {
    if (!permTarget) return;
    setSaving(true);
    try {
      await serviceKeysApi.update(permTarget.id, { allowed_models: [...permSet] });
      setPermDialogOpen(false);
      await fetchServiceKeys(true);
    } catch (e: any) {
      console.error('Failed to save perms:', e);
    } finally {
      setSaving(false);
    }
  };

  const handleSaveQuota = async () => {
    if (!quotaTarget) return;
    setSaving(true);
    try {
      await serviceKeysApi.update(quotaTarget.id, {
        quota_5h: quota5h ? parseInt(quota5h, 10) : 0,
        quota_7d: quota7d ? parseInt(quota7d, 10) : 0,
      });
      setQuotaDialogOpen(false);
      await fetchServiceKeys(true);
    } catch (e: any) {
      console.error('Failed to save quota:', e);
    } finally {
      setSaving(false);
    }
  };

  const confirmDelete = async () => {
    if (!deleteTarget) return;
    try {
      await serviceKeysApi.delete(deleteTarget.id);
      setDeleteDialogOpen(false);
      setDeleteTarget(null);
      await fetchServiceKeys(true);
    } catch (e: any) {
      console.error('Failed to delete service key:', e);
    }
  };

  const copyToClipboard = async (text: string) => {
    try {
      await navigator.clipboard.writeText(text);
      setCopied(true);
      setTimeout(() => setCopied(false), 2000);
      return true;
    } catch {
      return false;
    }
  };

  const chip = 'inline-flex items-center px-2 py-1 rounded-md bg-background border text-xs font-mono';

  return (
    <div className="space-y-6">
      {/* Header */}
      <div className="flex justify-between items-start gap-4 flex-wrap">
        <h2 className="text-3xl font-normal m-0">{t('keys.title')}</h2>
        <Button onClick={openCreate}>
          <Plus className="w-4 h-4 mr-2" />
          {t('keys.create')}
        </Button>
      </div>

      {/* Search */}
      <div className="relative flex-1 min-w-[200px] max-w-md">
        <Search className="absolute left-3 top-1/2 -translate-y-1/2 w-4 h-4 text-muted-foreground" />
        <input
          type="text"
          value={searchQuery}
          onChange={(e) => setSearchQuery(e.target.value)}
          placeholder={t('keys.search')}
          className="w-full h-10 pl-10 pr-4 rounded-md border border-input bg-background text-sm focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
        />
      </div>

      {/* Keys list */}
      {loading ? (
        <div className="flex items-center justify-center py-16">
          <Loader2 className="w-6 h-6 animate-spin text-muted-foreground" />
        </div>
      ) : filteredKeys.length === 0 ? (
        <div className="flex flex-col items-center gap-2 py-16 text-center">
          <KeyIcon className="w-12 h-12 text-muted-foreground" />
          <p className="text-lg">{t('common.empty')}</p>
        </div>
      ) : (
        <div className="border rounded-lg overflow-x-auto">
          {/* 真 table 而非独立 grid：行与表头共享列轨道，确保每列表头与数据左对齐 */}
          <table className="w-full min-w-[820px] text-left">
            <thead>
              <tr className="bg-muted text-xs text-muted-foreground border-b border-border">
                <th className="px-4 py-2.5 font-normal">{t('keys.col_key')}</th>
                <th className="px-3 py-2.5 font-normal">{t('keys.col_models')}</th>
                <th className="px-3 py-2.5 font-normal">{t('keys.col_quota')}</th>
                <th className="px-3 py-2.5 font-normal text-right">{t('keys.col_times')}</th>
                <th className="w-9" />
              </tr>
            </thead>
            <tbody>
              {filteredKeys.map((k) => {
                const lines = quotaLines(k, t);
                return (
                  <tr key={k.id} className="border-b border-border last:border-b-0 hover:bg-muted/40">
                    {/* 密钥：备注名 + 掩码 */}
                    <td className="px-4 py-3">
                      <p className="font-medium truncate" title={k.name}>
                        {k.name || t('common.unnamed')}
                      </p>
                      <p className="text-xs text-muted-foreground font-mono truncate mt-0.5">{k.key_masked}</p>
                    </td>

                    {/* 可用模型：白名单 chips；空 = 全部 */}
                    <td className="px-3 py-3">
                      <div className="flex flex-wrap gap-1">
                        {!k.allowed_models || k.allowed_models.length === 0 ? (
                          <span className={chip}>{t('common.all')}</span>
                        ) : (
                          k.allowed_models.map((m) => (
                            <span key={m} className={chip}>{m}</span>
                          ))
                        )}
                      </div>
                    </td>

                    {/* 限额：各窗口 已用% + 剩余；未设限显示 - */}
                    <td className="px-3 py-3">
                      <div className="flex flex-col gap-0.5">
                        {lines.length === 0 ? (
                          <span className="text-muted-foreground">-</span>
                        ) : (
                          lines.map((l) => (
                            <span
                              key={l.key}
                              className={cn(
                                'font-mono text-xs text-muted-foreground whitespace-nowrap',
                                l.over && 'text-destructive font-medium'
                              )}
                            >
                              {l.key}: {l.percent.toFixed(0)}% {l.resets_in}
                            </span>
                          ))
                        )}
                      </div>
                    </td>

                    {/* 创建 / 修改时间（右对齐） */}
                    <td className="px-3 py-3 text-right text-xs text-muted-foreground whitespace-nowrap">
                      <p>{formatTime(k.created_at)}</p>
                      <p>{formatTime(k.updated_at)}</p>
                    </td>

                    {/* Actions */}
                    <td className="pr-3 align-middle">
                      <DropdownMenu>
                        <DropdownMenuTrigger asChild>
                          <Button variant="ghost" size="icon" className="w-9 h-9">
                            <MoreVertical className="w-4 h-4" />
                          </Button>
                        </DropdownMenuTrigger>
                        <DropdownMenuContent align="end">
                          <DropdownMenuItem onClick={() => { setRenameTarget(k); setRenameName(k.name || ''); setRenameDialogOpen(true); }}>
                            <Pencil className="w-4 h-4 mr-2" />
                            {t('keys.rename')}
                          </DropdownMenuItem>
                          <DropdownMenuItem onClick={() => openPerms(k)}>
                            <Shield className="w-4 h-4 mr-2" />
                            {t('keys.edit_perm')}
                          </DropdownMenuItem>
                          <DropdownMenuItem onClick={() => { setQuotaTarget(k); setQuota5h(k.quota_5h?.toString() || ''); setQuota7d(k.quota_7d?.toString() || ''); setQuotaDialogOpen(true); }}>
                            <Gauge className="w-4 h-4 mr-2" />
                            {t('keys.config_quota')}
                          </DropdownMenuItem>
                          <DropdownMenuItem onClick={() => { setDeleteTarget(k); setDeleteDialogOpen(true); }} className="text-destructive focus:text-destructive">
                            <Trash2 className="w-4 h-4 mr-2" />
                            {t('common.delete')}
                          </DropdownMenuItem>
                        </DropdownMenuContent>
                      </DropdownMenu>
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        </div>
      )}

      {/* Create dialog */}
      <Dialog
        open={createDialogOpen}
        onOpenChange={(open) => {
          setCreateDialogOpen(open);
          if (!open) {
            setCreatedKey(null);
            setCopied(false);
          }
        }}
      >
        <DialogContent className="max-w-2xl">
          <DialogHeader>
            <DialogTitle>{t('keys.create')}</DialogTitle>
            <DialogDescription>{t('keys.perm_desc')}</DialogDescription>
          </DialogHeader>

          {createdKey ? (
            <div className="space-y-4">
              <div className="rounded-lg bg-green-500/10 border border-green-500/20 p-4">
                <p className="text-sm font-medium text-green-600 dark:text-green-400 mb-2">
                  {t('keys.created_once')}
                </p>
                <p className="text-xs text-muted-foreground mb-3">{t('keys.save_warning')}</p>
                <label className="text-xs font-medium">{t('keys.plain_key')}</label>
                <div className="flex gap-2 mt-1">
                  <input
                    type="text"
                    readOnly
                    value={createdKey.key}
                    className="flex-1 px-3 py-2 rounded-md border border-input bg-background text-sm font-mono"
                  />
                  <Button size="sm" onClick={() => copyToClipboard(createdKey.key)}>
                    {copied ? <Check className="w-4 h-4" /> : <Copy className="w-4 h-4" />}
                  </Button>
                </div>
                {deployLink && (
                  <>
                    <label className="text-xs font-medium mt-3 block">
                      {t('keys.deploy_link')}
                    </label>
                    <div className="flex gap-2 mt-1">
                      <input
                        type="text"
                        readOnly
                        value={deployLink}
                        className="flex-1 px-3 py-2 rounded-md border border-input bg-background text-sm font-mono"
                      />
                      <Button size="sm" onClick={() => copyToClipboard(deployLink)}>
                        {copied ? <Check className="w-4 h-4" /> : <Copy className="w-4 h-4" />}
                      </Button>
                    </div>
                  </>
                )}
              </div>
            </div>
          ) : (
            <div className="space-y-4">
              <div>
                <label className="text-sm font-medium">{t('keys.rename_label')}</label>
                <input
                  type="text"
                  value={newName}
                  onChange={(e) => setNewName(e.target.value)}
                  autoFocus
                  placeholder="my-claude-code"
                  className="w-full mt-1 h-10 px-3 rounded-md border border-input bg-background text-sm focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
                />
              </div>

              {/* 可用模型：创建时即可限定白名单（不勾选 = 全部） */}
              <div>
                <label className="text-sm font-medium">{t('keys.col_models')}</label>
                {modelsLoading ? (
                  <div className="flex justify-center py-6">
                    <Loader2 className="w-5 h-5 animate-spin text-muted-foreground" />
                  </div>
                ) : permGroups.length === 0 ? (
                  <p className="text-sm text-muted-foreground py-3">{t('keys.perm_no_models')}</p>
                ) : (
                  <div className="max-h-[32vh] overflow-y-auto space-y-4 pr-2 mt-2 border border-border rounded-md p-3">
                    {permGroups.map((g) => (
                      <div key={g.name}>
                        <p className="text-xs font-medium text-muted-foreground mb-1.5">{g.name}</p>
                        <div className="grid grid-cols-2 md:grid-cols-3 gap-x-4 gap-y-1.5">
                          {g.models.map((m) => {
                            const checked = createSet.has(m);
                            return (
                              <label
                                key={m}
                                className="flex items-center gap-2 text-sm cursor-pointer select-none"
                              >
                                <input
                                  type="checkbox"
                                  checked={checked}
                                  onChange={() => toggleCreatePerm(m)}
                                  className="h-4 w-4 rounded border-input accent-primary"
                                />
                                <span className="font-mono text-xs truncate">{m}</span>
                              </label>
                            );
                          })}
                        </div>
                      </div>
                    ))}
                  </div>
                )}
              </div>
            </div>
          )}

          <DialogFooter>
            {createdKey ? (
              <Button onClick={() => setCreateDialogOpen(false)}>{t('common.done')}</Button>
            ) : (
              <>
                <Button variant="outline" onClick={() => setCreateDialogOpen(false)}>
                  {t('common.cancel')}
                </Button>
                <Button onClick={handleCreate} disabled={creating}>
                  {creating && <Loader2 className="w-4 h-4 mr-2 animate-spin" />}
                  {t('common.create')}
                </Button>
              </>
            )}
          </DialogFooter>
        </DialogContent>
      </Dialog>

      {/* Rename dialog */}
      <Dialog open={renameDialogOpen} onOpenChange={setRenameDialogOpen}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>{t('keys.rename_title')}</DialogTitle>
          </DialogHeader>
          <div>
            <label className="text-sm font-medium">{t('keys.rename_label')}</label>
            <input
              type="text"
              value={renameName}
              onChange={(e) => setRenameName(e.target.value)}
              autoFocus
              className="w-full mt-1 h-10 px-3 rounded-md border border-input bg-background text-sm focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
            />
          </div>
          <DialogFooter>
            <Button variant="outline" onClick={() => setRenameDialogOpen(false)}>
              {t('common.cancel')}
            </Button>
            <Button onClick={handleRename} disabled={saving}>
              {saving && <Loader2 className="w-4 h-4 mr-2 animate-spin" />}
              {t('common.save')}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      {/* Perm dialog */}
      <Dialog open={permDialogOpen} onOpenChange={setPermDialogOpen}>
        <DialogContent className="max-w-2xl">
          <DialogHeader>
            <DialogTitle>{t('keys.perm_title', { name: permTarget?.name || t('common.unnamed') })}</DialogTitle>
            <DialogDescription>{t('keys.perm_desc')}</DialogDescription>
          </DialogHeader>

          {modelsLoading ? (
            <div className="flex justify-center py-8">
              <Loader2 className="w-6 h-6 animate-spin text-muted-foreground" />
            </div>
          ) : permGroups.length === 0 ? (
            <p className="text-sm text-muted-foreground py-4">{t('keys.perm_no_models')}</p>
          ) : (
            <div className="max-h-[40vh] overflow-y-auto space-y-4 pr-2">
              {permGroups.map((g) => (
                <div key={g.name}>
                  <p className="text-xs font-medium text-muted-foreground mb-1.5">{g.name}</p>
                  <div className="grid grid-cols-2 md:grid-cols-3 gap-x-4 gap-y-1.5">
                    {g.models.map((m) => {
                      const checked = permSet.has(m);
                      return (
                        <label
                          key={m}
                          className="flex items-center gap-2 text-sm cursor-pointer select-none"
                        >
                          <input
                            type="checkbox"
                            checked={checked}
                            onChange={() => togglePerm(m)}
                            className="h-4 w-4 rounded border-input accent-primary"
                          />
                          <span className="font-mono text-xs truncate">{m}</span>
                        </label>
                      );
                    })}
                  </div>
                </div>
              ))}
            </div>
          )}

          <DialogFooter>
            <Button variant="outline" onClick={() => setPermDialogOpen(false)}>
              {t('common.cancel')}
            </Button>
            <Button onClick={handleSavePerms} disabled={saving}>
              {saving && <Loader2 className="w-4 h-4 mr-2 animate-spin" />}
              {t('common.save')}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      {/* Quota dialog */}
      <Dialog open={quotaDialogOpen} onOpenChange={setQuotaDialogOpen}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>{t('keys.quota_title', { name: quotaTarget?.name || t('common.unnamed') })}</DialogTitle>
            <DialogDescription>{t('keys.quota_desc')}</DialogDescription>
          </DialogHeader>
          <div className="space-y-4">
            <div>
              <label className="text-sm font-medium">{t('keys.quota_5h_label')}</label>
              <input
                type="number"
                min={0}
                value={quota5h}
                onChange={(e) => setQuota5h(e.target.value)}
                placeholder="0"
                className="w-full mt-1 h-10 px-3 rounded-md border border-input bg-background text-sm font-mono focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
              />
              <p className="text-xs text-muted-foreground mt-1">
                {t('keys.quota_preview', { value: formatAbbrev(parseInt(quota5h || '0', 10) || 0, t) })}
              </p>
            </div>
            <div>
              <label className="text-sm font-medium">{t('keys.quota_7d_label')}</label>
              <input
                type="number"
                min={0}
                value={quota7d}
                onChange={(e) => setQuota7d(e.target.value)}
                placeholder="0"
                className="w-full mt-1 h-10 px-3 rounded-md border border-input bg-background text-sm font-mono focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
              />
              <p className="text-xs text-muted-foreground mt-1">
                {t('keys.quota_preview', { value: formatAbbrev(parseInt(quota7d || '0', 10) || 0, t) })}
              </p>
            </div>
          </div>
          <DialogFooter>
            <Button variant="outline" onClick={() => setQuotaDialogOpen(false)}>
              {t('common.cancel')}
            </Button>
            <Button onClick={handleSaveQuota} disabled={saving}>
              {saving && <Loader2 className="w-4 h-4 mr-2 animate-spin" />}
              {t('common.save')}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      {/* Delete dialog */}
      <Dialog open={deleteDialogOpen} onOpenChange={setDeleteDialogOpen}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>{t('keys.delete_title')}</DialogTitle>
            <DialogDescription>
              {t('keys.delete_confirm', { name: deleteTarget?.name || t('common.unnamed') })}
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button variant="outline" onClick={() => setDeleteDialogOpen(false)}>
              {t('common.cancel')}
            </Button>
            <Button variant="destructive" onClick={confirmDelete}>
              {t('keys.delete_confirm_btn')}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}

export default KeysView;
