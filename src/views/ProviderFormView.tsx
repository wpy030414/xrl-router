import { useState, useEffect, useRef } from 'react';
import { useNavigate, useParams, useSearchParams } from 'react-router';
import { ArrowLeft, Loader2, Pencil, Plus, Trash2 } from 'lucide-react';
import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';
import { Label } from '@/components/ui/label';
import { Textarea } from '@/components/ui/textarea';
import { Alert } from '@/components/ui/alert';
import { Switch } from '@/components/ui/switch';
import {
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog';
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select';
import { useProvidersStore } from '@/stores/providers';
import { useApiKeysStore } from '@/stores/apiKeys';
import { providersApi, pluginsApi, modelsApi, keysApi, type Provider, type PluginDetail } from '@/lib/api';
import { useT } from '@/i18n';
import { cn } from '@/lib/utils';

/** Provider 类型选项 */
const KIND_OPTIONS = [
  { value: 'messages', labelKey: 'providerForm.kind.messages' },
  { value: 'chat_completions', labelKey: 'providerForm.kind.chat_completions' },
  { value: 'responses', labelKey: 'providerForm.kind.responses' },
] as const;

/** 默认 base URL */
const DEFAULT_URLS: Record<string, string> = {
  messages: 'https://api.anthropic.com',
  chat_completions: 'https://api.openai.com',
  responses: 'https://api.openai.com',
};

/** 默认 API path */
const DEFAULT_PATHS: Record<string, string> = {
  messages: '/v1/messages',
  chat_completions: '/v1/chat/completions',
  responses: '/v1/responses',
};

/** 模型草稿行：增删/改名/启停全部是草稿操作，点总表单"保存"时统一对账到 models 表 */
interface DraftModel {
  /** 行渲染 key（服务端行直接用行 id，新行用本地自增 key） */
  key: string;
  model_id: string;
  display_name: string;
  enabled: boolean;
}

/** 解析密钥文本为明文密钥数组（一行一个，忽略空行）。 */
function parseKeysText(text: string): string[] {
  return text
    .split('\n')
    .map((line) => line.trim())
    .filter(Boolean);
}

export function ProviderFormView() {
  const t = useT();
  const navigate = useNavigate();
  const { id } = useParams<{ id: string }>();
  const [searchParams] = useSearchParams();
  const { fetchProviders } = useProvidersStore();
  const { createKey } = useApiKeysStore();

  const isEdit = !!id;
  const queryPluginId = searchParams.get('plugin_id');
  // 编辑已有插件供应商时同样进入插件模式（config.plugin_id 识别）
  const [editPluginId, setEditPluginId] = useState<string | null>(null);
  const pluginId = queryPluginId ?? editPluginId;
  const isPlugin = !!pluginId;

  // Form state
  const [name, setName] = useState('');
  const [kind, setKind] = useState<Provider['kind']>('messages');
  const [baseUrl, setBaseUrl] = useState(DEFAULT_URLS.messages);
  const [apiPath, setApiPath] = useState(DEFAULT_PATHS.messages);
  const [apiKeysText, setApiKeysText] = useState('');
  // 模型草稿列表（统一数据源：新增/编辑/删除/行级启停都只改草稿，保存后对账生效）
  const [draftModels, setDraftModels] = useState<DraftModel[]>([]);
  // 新行 key 自增序号（ref 保证重渲染间稳定）
  const localKeySeq = useRef(0);
  const nextLocalKey = () => `local-${++localKeySeq.current}`;
  // 插件项目目录（workdir）：伴生启动 `pnpm run serve/login` 的 cwd
  const [workDir, setWorkDir] = useState('');
  const [saving, setSaving] = useState(false);
  // 仅在需要拉取远端数据（编辑 / 插件预填）时进入加载态；新建直接渲染表单。
  const [loading, setLoading] = useState(isEdit || isPlugin);
  const [error, setError] = useState<string | null>(null);
  const [pluginInfo, setPluginInfo] = useState<PluginDetail | null>(null);

  // 模型新增/编辑 dialog 状态（editingModelKey 为 null = 新增模式）
  const [modelDialogOpen, setModelDialogOpen] = useState(false);
  const [editingModelKey, setEditingModelKey] = useState<string | null>(null);
  const [dialogModelId, setDialogModelId] = useState('');
  const [dialogAlias, setDialogAlias] = useState('');
  const [modelDialogError, setModelDialogError] = useState<string | null>(null);

  // Load existing provider data for edit mode
  useEffect(() => {
    if (!isEdit) return;

    const load = async () => {
      setLoading(true);
      try {
        const provider = await providersApi.get(id);
        // 插件供应商（config_json 含 plugin_id）：同样进入插件模式——
        // 隐藏 API Key 输入、禁用 kind/base_url；名字保持可编辑
        const cfgPluginId = provider.config?.plugin_id as string | undefined;
        if (cfgPluginId) {
          setEditPluginId(cfgPluginId);
          // workdir 来自 plugins 表（register 上报 / 用户编辑）
          try {
            const detail = await pluginsApi.get(cfgPluginId);
            setWorkDir(detail.work_dir || '');
          } catch {
            // 插件记录缺失不阻断表单（provider 数据仍可编辑）
          }
        }
        setName(provider.name);
        setKind(provider.kind);
        setBaseUrl(provider.base_url);
        setApiPath(provider.api_path);
        // 模型以 models 表为准（代理按该表路由）；config.models 是旧版
        // 遗留数据，仅在表里没有数据时作兼容回退（视为新增行，保存时入库）。
        const dbModels = await modelsApi.list(id);
        if (dbModels.length > 0) {
          setDraftModels(
            dbModels.map((m) => ({
              key: m.id,
              model_id: m.model_id,
              display_name: m.display_name,
              enabled: m.enabled,
            })),
          );
        } else {
          const legacy = (provider.config?.models || []) as { model_id: string; display_name: string }[];
          setDraftModels(
            legacy.map((m) => ({
              key: nextLocalKey(),
              model_id: m.model_id,
              display_name: m.display_name || m.model_id,
              enabled: true,
            })),
          );
        }
        // 回填明文密钥（一行一个）；插件模式无密钥（凭证由插件方持有）
        if (!provider.config?.plugin_id) {
          const keys = await keysApi.list(id);
          setApiKeysText(keys.map((k) => k.key_plain || '').filter(Boolean).join('\n'));
        }
      } catch (e: any) {
        setError(t('providerForm.load_failed', { msg: e.message }));
      } finally {
        setLoading(false);
      }
    };

    load();
  }, [id, isEdit, t]);

  // Load plugin info for plugin mode（仅弹窗跳转的查询串模式触发；
  // 编辑模式的数据由上方编辑 effect 从 provider + models 表回填）
  useEffect(() => {
    if (!queryPluginId) return;

    const load = async () => {
      setLoading(true);
      try {
        const data = await pluginsApi.get(queryPluginId);
        setPluginInfo(data);
        setName(data.provider.name || queryPluginId);
        setKind((data.provider.kind as Provider['kind']) || 'chat_completions');
        setBaseUrl(data.provider.base_url || '');
        setApiPath(data.provider.api_path || DEFAULT_PATHS[data.provider.kind] || '');
        setWorkDir(data.work_dir || '');
        // 插件自带模型列表（注册时已写入 models 表），预填为草稿行，保存时对账
        setDraftModels(
          (data.models || []).map((m) => ({
            key: nextLocalKey(),
            model_id: m.model_id,
            display_name: m.display_name || m.model_id,
            enabled: true,
          })),
        );
      } catch (e: any) {
        setError(t('providerForm.plugin_load_failed', { msg: e.message }));
      } finally {
        setLoading(false);
      }
    };

    load();
  }, [queryPluginId]);

  // Update default URL and path when kind changes (only in create mode)
  const handleKindChange = (newKind: Provider['kind']) => {
    setKind(newKind);
    if (!isEdit && !isPlugin) {
      setBaseUrl(DEFAULT_URLS[newKind]);
      setApiPath(DEFAULT_PATHS[newKind]);
    }
  };

  // ---- 模型草稿操作：只改本地状态，等总表单"保存"统一对账生效 ----

  const toggleDraftEnabled = (key: string, next: boolean) => {
    setDraftModels((prev) =>
      prev.map((m) => (m.key === key ? { ...m, enabled: next } : m)),
    );
  };

  const removeDraft = (key: string) => {
    setDraftModels((prev) => prev.filter((m) => m.key !== key));
  };

  const openAddModelDialog = () => {
    setEditingModelKey(null);
    setDialogModelId('');
    setDialogAlias('');
    setModelDialogError(null);
    setModelDialogOpen(true);
  };

  const openEditModelDialog = (m: DraftModel) => {
    setEditingModelKey(m.key);
    // 别名与模型名相同时视为"未设置"，输入框留空更直观
    setDialogModelId(m.model_id);
    setDialogAlias(m.display_name === m.model_id ? '' : m.display_name);
    setModelDialogError(null);
    setModelDialogOpen(true);
  };

  const confirmModelDialog = () => {
    const mid = dialogModelId.trim();
    const alias = dialogAlias.trim();
    if (!mid) {
      setModelDialogError(t('providerForm.model_dialog.empty_error'));
      return;
    }
    // model_id 是对账主键，禁止与本供应商其他行重复
    if (draftModels.some((m) => m.key !== editingModelKey && m.model_id === mid)) {
      setModelDialogError(t('providerForm.model_dialog.duplicate_error'));
      return;
    }
    setDraftModels((prev) => {
      if (editingModelKey) {
        return prev.map((m) =>
          m.key === editingModelKey ? { ...m, model_id: mid, display_name: alias || mid } : m,
        );
      }
      return [...prev, { key: nextLocalKey(), model_id: mid, display_name: alias || mid, enabled: true }];
    });
    setModelDialogOpen(false);
  };

  const handleSave = async () => {
    if (!name.trim()) return;

    setSaving(true);
    setError(null);

    try {
      const models = draftModels.map((m) => ({
        model_id: m.model_id,
        display_name: m.display_name || m.model_id,
      }));
      // 插件模式：config 与注册时保持一致（PUT 整包替换 config，必须带全）；
      // models 以表为准，config.models 已是旧版遗留（Vue 版同样不带）
      const config: Record<string, any> = isPlugin
        ? { plugin_id: pluginId!, delegated: true }
        : { models };

      const data: Partial<Provider> = {
        name: name.trim(),
        kind,
        base_url: baseUrl.trim(),
        api_path: apiPath.trim(),
        enabled: true,
        config,
      };

      let savedProvider: Provider;
      let needConfirm = false;

      if (isPlugin && !isEdit) {
        // 插件模式：provider 已在注册时创建，不重复创建，只更新
        if (!pluginInfo?.provider.id) throw new Error('plugin info missing');
        savedProvider = await providersApi.update(pluginInfo.provider.id, data);
        needConfirm = true;
      } else if (isEdit) {
        savedProvider = await providersApi.update(id, data);
      } else {
        savedProvider = await providersApi.create(data);
      }

      // 插件模式：同步 work_dir 到 plugins 表（空串 = 清除；伴生启动的 cwd）。
      // 失败非致命——provider 已保存，目录可稍后在卡片菜单的伴生启动入口修正。
      if (isPlugin && pluginId) {
        try {
          await pluginsApi.update(pluginId, { work_dir: workDir });
        } catch (e: any) {
          console.error('Plugin work_dir sync failed:', e.message);
        }
      }

      // 模型草稿全量对账到 models 表（代理按该表路由，config.models 只是记录）：
      // 删除草稿里移除的、新增缺失的、同步别名改名与启停——已在用的模型
      // 不做删旧建新，避免 usage_log 的 model_id 外键引用被清掉。
      // 注意 create 固定 enabled=true，新增即停用的行需补一次 update。
      const existingModels = await modelsApi.list(savedProvider.id);
      const wantIds = new Set(draftModels.map((m) => m.model_id));
      const seenIds = new Set<string>();
      for (const m of existingModels) {
        if (!wantIds.has(m.model_id)) {
          await modelsApi.delete(m.id);
        }
      }
      for (const draft of draftModels) {
        if (seenIds.has(draft.model_id)) continue; // 防御：脏数据重复行只对账一次
        seenIds.add(draft.model_id);
        const displayName = draft.display_name || draft.model_id;
        const ex = existingModels.find((e) => e.model_id === draft.model_id);
        if (!ex) {
          const created = await modelsApi.create({
            provider_id: savedProvider.id,
            model_id: draft.model_id,
            display_name: displayName,
            tier: 'custom',
          });
          if (!draft.enabled) {
            await modelsApi.update(created.id, { enabled: false });
          }
        } else {
          if (ex.display_name !== displayName) {
            await modelsApi.update(ex.id, { display_name: displayName });
          }
          if (ex.enabled !== draft.enabled) {
            await modelsApi.update(ex.id, { enabled: draft.enabled });
          }
        }
      }

      // API Key（一行一个）全量对账：新增缺失的、删除从输入里移除的；
      // 明文相同的保留原 key id，不破坏用量统计归属。插件模式跳过——
      // V24 契约下 Router 不为插件管密钥，凭证由插件方 login 流程持有。
      if (!isPlugin) {
        try {
          const inputKeys = parseKeysText(apiKeysText);
          const existingKeys = await keysApi.list(savedProvider.id);
          const existingPlain = new Set(existingKeys.map((k) => k.key_plain || '').filter(Boolean));
          for (const line of inputKeys) {
            if (!existingPlain.has(line)) {
              await createKey(savedProvider.id, {
                name: name.trim() + t('providerForm.key_suffix'),
                key: line,
              });
            }
          }
          for (const k of existingKeys) {
            if (k.key_plain && !inputKeys.includes(k.key_plain)) {
              await keysApi.delete(savedProvider.id, k.id);
            }
          }
        } catch (e: any) {
          // Key sync failure is non-fatal — provider is already saved
          console.error('Key sync failed:', e.message);
        }
      }

      // 插件模式首次添加：确认激活插件供应商（编辑模式不重复确认）
      if (needConfirm) {
        await pluginsApi.confirm(pluginId!);
      }

      await fetchProviders();
      navigate('/providers');
    } catch (e: any) {
      setError(t('providerForm.save_failed', { msg: e.message }));
    } finally {
      setSaving(false);
    }
  };

  const title = isEdit && isPlugin
    ? t('providerForm.title.plugin_edit')
    : isPlugin
    ? t('providerForm.title.plugin')
    : isEdit
    ? t('providerForm.title.edit')
    : t('providerForm.title.create');

  if (loading) {
    return (
      <div className="flex items-center justify-center py-16">
        <Loader2 className="w-6 h-6 animate-spin text-muted-foreground" />
      </div>
    );
  }

  return (
    <div className="space-y-6">
      {/* Header */}
      <div className="flex items-center gap-3">
        <Button variant="ghost" size="icon" onClick={() => navigate('/providers')}>
          <ArrowLeft className="w-5 h-5" />
        </Button>
        <h2 className="text-3xl font-normal m-0">{title}</h2>
      </div>

      {/* Error banner */}
      {error && (
        <Alert variant="destructive">
          {error}
        </Alert>
      )}

      {/* Form */}
      <div className="space-y-5">
        {/* Name */}
        <div className="space-y-1.5">
          <Label htmlFor="provider-name">
            {t('providerForm.name_label')}
          </Label>
          <Input
            id="provider-name"
            type="text"
            value={name}
            onChange={(e) => setName(e.target.value)}
            placeholder={t('providerForm.name_placeholder')}
          />
        </div>

        {/* Kind */}
        <div className="space-y-1.5">
          <Label htmlFor="provider-kind">
            {t('providerForm.kind_label')}
          </Label>
          <Select
            value={kind}
            onValueChange={(v) => handleKindChange(v as Provider['kind'])}
            disabled={isEdit || isPlugin}
          >
            <SelectTrigger id="provider-kind">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              {KIND_OPTIONS.map((opt) => (
                <SelectItem key={opt.value} value={opt.value}>
                  {t(opt.labelKey)}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </div>

        {/* Base URL */}
        <div className="space-y-1.5">
          <Label htmlFor="provider-base-url">
            {t('providerForm.base_url_label')}
          </Label>
          <Input
            id="provider-base-url"
            type="url"
            value={baseUrl}
            onChange={(e) => setBaseUrl(e.target.value)}
            disabled={isPlugin}
            className="font-mono"
            placeholder={t('providerForm.base_url_placeholder')}
          />
        </div>

        {/* 插件模式：项目目录（workdir）——伴生启动 `pnpm run serve/login` 的 cwd */}
        {isPlugin && (
          <div className="space-y-1.5">
            <Label htmlFor="provider-workdir">
              {t('providerForm.workdir_label')}
            </Label>
            <Input
              id="provider-workdir"
              type="text"
              value={workDir}
              onChange={(e) => setWorkDir(e.target.value)}
              className="font-mono"
              placeholder={t('providerForm.workdir_placeholder')}
            />
            <p className="text-xs text-muted-foreground">
              {t('providerForm.workdir_hint')}
            </p>
          </div>
        )}

        {/* API Key（一行一个）。仅非插件模式渲染——V24 契约下插件的凭证
            由插件方 login 流程持有，Router 不接管密钥 */}
        {!isPlugin && (
          <div className="space-y-1.5">
            <Label htmlFor="provider-api-key">
              {t('providerForm.api_key_label')}
            </Label>
            <Textarea
              id="provider-api-key"
              value={apiKeysText}
              onChange={(e) => setApiKeysText(e.target.value)}
              rows={3}
              autoComplete="off"
              spellCheck={false}
              className="font-mono"
              placeholder={t('providerForm.api_key_placeholder')}
            />
          </div>
        )}

        {/* Models：统一草稿列表（沿用"已注册模型"行样式）。增删/编辑/启停
            全部是草稿操作，需点击底部保存按钮统一生效 */}
        <div className="space-y-1.5">
          <div className="flex items-center justify-between">
            <Label>{t('providerForm.models_label')}</Label>
            <Button
              variant="ghost"
              size="icon"
              onClick={openAddModelDialog}
              title={t('providerForm.model_dialog.add_title')}
              aria-label={t('providerForm.model_dialog.add_title')}
            >
              <Plus className="w-4 h-4" />
            </Button>
          </div>
          {draftModels.length === 0 ? (
            <div className="border border-dashed rounded-md px-3 py-6 text-center text-xs text-muted-foreground">
              {t('providerForm.models_empty')}
            </div>
          ) : (
            <div className="border rounded-md divide-y divide-border">
              {draftModels.map((m) => (
                <div key={m.key} className="flex items-center gap-2 px-3 py-2">
                  <div className="flex-1 min-w-0">
                    <p className="font-mono text-sm truncate" title={m.display_name}>
                      {m.display_name || m.model_id}
                    </p>
                    {m.display_name && m.display_name !== m.model_id && (
                      <p className="text-xs text-muted-foreground font-mono truncate">
                        {m.model_id}
                      </p>
                    )}
                  </div>
                  <span
                    className={cn(
                      'text-xs shrink-0',
                      m.enabled ? 'text-muted-foreground' : 'text-destructive',
                    )}
                  >
                    {m.enabled
                      ? t('providerForm.model_state_on')
                      : t('providerForm.model_state_off')}
                  </span>
                  <Switch
                    checked={m.enabled}
                    onCheckedChange={(v) => toggleDraftEnabled(m.key, v)}
                  />
                  <Button
                    variant="ghost"
                    size="icon"
                    className="h-7 w-7 shrink-0"
                    onClick={() => openEditModelDialog(m)}
                    title={t('common.edit')}
                    aria-label={t('common.edit')}
                  >
                    <Pencil className="w-3.5 h-3.5" />
                  </Button>
                  <Button
                    variant="ghost"
                    size="icon"
                    className="h-7 w-7 shrink-0 text-muted-foreground hover:text-destructive"
                    onClick={() => removeDraft(m.key)}
                    title={t('common.delete')}
                    aria-label={t('common.delete')}
                  >
                    <Trash2 className="w-3.5 h-3.5" />
                  </Button>
                </div>
              ))}
            </div>
          )}
        </div>

        {/* 模型新增/编辑弹窗 */}
        <Dialog open={modelDialogOpen} onOpenChange={setModelDialogOpen}>
          <DialogContent className="max-w-sm">
            <DialogHeader>
              <DialogTitle>
                {editingModelKey
                  ? t('providerForm.model_dialog.edit_title')
                  : t('providerForm.model_dialog.add_title')}
              </DialogTitle>
            </DialogHeader>
            <form
              className="space-y-3"
              onSubmit={(e) => {
                e.preventDefault();
                confirmModelDialog();
              }}
            >
              <div className="space-y-1.5">
                <Label htmlFor="model-dialog-id">
                  {t('providerForm.model_dialog.id_label')}
                </Label>
                <Input
                  id="model-dialog-id"
                  autoFocus
                  value={dialogModelId}
                  onChange={(e) => setDialogModelId(e.target.value)}
                  className="font-mono"
                  spellCheck={false}
                  placeholder={t('providerForm.model_dialog.id_placeholder')}
                />
              </div>
              <div className="space-y-1.5">
                <Label htmlFor="model-dialog-alias">
                  {t('providerForm.model_dialog.alias_label')}
                </Label>
                <Input
                  id="model-dialog-alias"
                  value={dialogAlias}
                  onChange={(e) => setDialogAlias(e.target.value)}
                  className="font-mono"
                  spellCheck={false}
                  placeholder={t('providerForm.model_dialog.alias_placeholder')}
                />
                <p className="text-xs text-muted-foreground">
                  {t('providerForm.model_dialog.alias_hint')}
                </p>
              </div>
              {modelDialogError && (
                <p className="text-xs text-destructive">{modelDialogError}</p>
              )}
              <DialogFooter>
                <Button type="button" variant="outline" onClick={() => setModelDialogOpen(false)}>
                  {t('common.cancel')}
                </Button>
                <Button type="submit" disabled={!dialogModelId.trim()}>
                  {editingModelKey
                    ? t('common.save')
                    : t('providerForm.model_dialog.add_confirm')}
                </Button>
              </DialogFooter>
            </form>
          </DialogContent>
        </Dialog>

        {/* Actions */}
        <div className="flex items-center gap-3 pt-2">
          <Button variant="outline" onClick={() => navigate('/providers')}>
            {t('common.cancel')}
          </Button>
          <Button onClick={handleSave} disabled={saving || !name.trim()}>
            {saving && <Loader2 className="w-4 h-4 mr-2 animate-spin" />}
            {saving
              ? t('providerForm.saving')
              : isEdit
              ? t('providerForm.save_edit')
              : t('providerForm.save_create')}
          </Button>
        </div>
      </div>
    </div>
  );
}

export default ProviderFormView;
