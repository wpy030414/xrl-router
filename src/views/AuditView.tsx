import { useState, useEffect, useCallback, useRef } from 'react';
import { ChevronLeft, ChevronRight, ChevronDown, Inbox, Loader2, Bot, User, MoreVertical, Trash2, FileDown } from 'lucide-react';
import { Button } from '@/components/ui/button';
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogDescription,
  DialogFooter,
} from '@/components/ui/dialog';
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu';
import { useT } from '@/i18n';
import { useWebSocket } from '@/hooks/useWebSocket';
import { auditApi, serviceKeysApi, type ConversationListItem, type ConversationDetail, type ServiceKey } from '@/lib/api';
import { tauriDialog, tauriFs, isTauri } from '@/lib/tauri';
import { cn } from '@/lib/utils';

const PAGE_SIZE = 20;

// Module-level: persists across renders for the session lifetime, resets on app restart.
let auditDeveloperMode = false;

/** 从 content block 数组中提取纯文本预览 */
function extractTextFromBlocks(blocks: unknown[], hideSystemBlocks: boolean): string {
  if (!Array.isArray(blocks)) return '';
  let text = blocks
    .filter((b: any) => b?.type === 'text' && typeof b?.text === 'string')
    .map((b: any) => b.text)
    .join('');
  if (hideSystemBlocks) {
    // Strip system-injected tags that end users should never see
    text = text
      .replace(/<system-reminder>[\s\S]*?<\/system-reminder>\s*/g, '')
      .replace(/<total_tokens>[\s\S]*?<\/total_tokens>\s*/g, '')
      .replace(/<local-command-caveat>[\s\S]*?<\/local-command-caveat>\s*/g, '')
      .replace(/<local-command-stdout>[\s\S]*?<\/local-command-stdout>\s*/g, '')
      .replace(/<task-notification>[\s\S]*?<\/task-notification>\s*/g, '');
  }
  return text.trim();
}

/** 格式化相对时间 */
function formatTime(ts: number): string {
  const d = new Date(ts * 1000);
  const now = Date.now();
  const diff = now - d.getTime();
  if (diff < 60_000) return `${Math.floor(diff / 1000)}s ago`;
  if (diff < 3_600_000) return `${Math.floor(diff / 60_000)}m ago`;
  if (diff < 86_400_000) return `${Math.floor(diff / 3_600_000)}h ago`;
  return d.toLocaleDateString(undefined, { month: 'short', day: 'numeric', hour: '2-digit', minute: '2-digit' });
}

export function AuditView() {
  const t = useT();
  const [conversations, setConversations] = useState<ConversationListItem[]>([]);
  const [total, setTotal] = useState(0);
  const [page, setPage] = useState(1);
  const [loading, setLoading] = useState(true);
  const [selectedConv, setSelectedConv] = useState<ConversationDetail | null>(null);
  const [keyFilter, setKeyFilter] = useState('');
  const [serviceKeys, setServiceKeys] = useState<ServiceKey[]>([]);
  const [detailLoading, setDetailLoading] = useState(false);
  const detailRef = useRef<HTMLDivElement>(null);

  // Developer mode: right-click title to toggle between simple/raw mode
  const [developerMode, setDeveloperMode] = useState(auditDeveloperMode);
  const [contextMenu, setContextMenu] = useState<{ x: number; y: number } | null>(null);

  // Delete confirmation dialog
  const [deleteTarget, setDeleteTarget] = useState<ConversationListItem | null>(null);
  const [deleteDialogOpen, setDeleteDialogOpen] = useState(false);
  const [deleting, setDeleting] = useState(false);

  const handleContextMenu = (e: React.MouseEvent) => {
    e.preventDefault();
    setContextMenu({ x: e.clientX, y: e.clientY });
  };

  const handleToggleMode = () => {
    const isRaw = !developerMode;
    auditDeveloperMode = isRaw;
    setDeveloperMode(isRaw);
    setContextMenu(null);
  };

  /** Build a plain-text rendering of the conversation for export / save-as. */
  const buildConversationText = (conv: ConversationDetail): string => {
    const lines: string[] = [];
    const ts = new Date(conv.created_at * 1000);
    lines.push(`# ${conv.service_key_name}`);
    lines.push(`# ${ts.toLocaleString()}`);
    lines.push(`# ${conv.message_count} messages · ${conv.request_count} requests`);
    lines.push('');
    for (const msg of conv.messages) {
      const role = msg.role === 'user' ? t('audit.role_user') : t('audit.role_assistant');
      const text = extractTextFromBlocks(msg.content as unknown[], /* hideSystemBlocks */ true);
      const images = Array.isArray(msg.content)
        ? (msg.content as any[]).filter((b: any) => b?.type === 'image').length
        : 0;
      lines.push(`## ${role}`);
      if (text) lines.push(text);
      if (images > 0) lines.push(`[${t('audit.image_placeholder')}] ×${images}`);
      if (!text && images === 0) lines.push('…');
      lines.push('');
    }
    return lines.join('\n');
  };

  const handleSaveAsTxt = async (conv: ConversationDetail) => {
    try {
      const text = buildConversationText(conv);
      const safeDate = new Date(conv.created_at * 1000).toISOString().split('T')[0];
      const safeKey = (conv.service_key_name || 'audit').replace(/[\\/:*?"<>|]/g, '_');
      const defaultName = `audit-${safeKey}-${safeDate}-${conv.id}.txt`;
      if (isTauri()) {
        const path = await tauriDialog.save({
          title: t('audit.save_as'),
          defaultPath: defaultName,
          filters: [{ name: 'Text', extensions: ['txt'] }],
        });
        if (path) await tauriFs.writeTextFile(path, text);
      } else {
        const blob = new Blob([text], { type: 'text/plain;charset=utf-8' });
        const url = URL.createObjectURL(blob);
        const a = document.createElement('a');
        a.href = url;
        a.download = defaultName;
        a.click();
        URL.revokeObjectURL(url);
      }
    } catch (e) {
      console.error('Save as TXT failed:', e);
    }
  };

  /** Save-as from the list row: fetch detail on demand, then export. */
  const handleSaveAsTxtById = async (id: number) => {
    try {
      const detail = await auditApi.get(id);
      await handleSaveAsTxt(detail);
    } catch (e) {
      console.error('Save as TXT failed:', e);
    }
  };

  const handleDeleteClick = (conv: ConversationListItem) => {
    setDeleteTarget(conv);
    setDeleteDialogOpen(true);
  };

  const handleConfirmDelete = async () => {
    if (!deleteTarget) return;
    setDeleting(true);
    try {
      await auditApi.delete(deleteTarget.id);
      // If the detail dialog is open for the deleted conversation, close it.
      if (selectedConv?.id === deleteTarget.id) setSelectedConv(null);
      setDeleteDialogOpen(false);
      setDeleteTarget(null);
      // Refresh current page; if page becomes empty, step back.
      await fetchConversations(page, true);
    } catch {
      // silent
    } finally {
      setDeleting(false);
    }
  };

  // Scroll to bottom whenever the detail dialog opens or its content changes.
  // Radix Dialog renders via Portal with a 200ms CSS animation, so the DOM
  // element may not be sized yet on mount. We rely on onAnimationEnd on the
  // DialogContent to trigger the first scroll after animation completes.
  const scrollDetailToBottom = useCallback(() => {
    if (detailRef.current) {
      detailRef.current.scrollTop = detailRef.current.scrollHeight;
    }
  }, []);


  const totalPages = Math.ceil(total / PAGE_SIZE);

  const fetchConversations = async (p: number, silent = false) => {
    if (!silent) setLoading(true);
    try {
      const res = await auditApi.list({ page: p, page_size: PAGE_SIZE, service_key_id: keyFilter || undefined });
      setConversations(res.data);
      setTotal(res.total);
    } catch {
      // silent
    } finally {
      if (!silent) setLoading(false);
    }
  };

  const fetchDetail = async (id: number) => {
    setDetailLoading(true);
    try {
      const detail = await auditApi.get(id);
      setSelectedConv(detail);
    } catch {
      // silent
    } finally {
      setDetailLoading(false);
    }
  };

  // WS real-time refresh (same pattern as StatsView)
  const refreshTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  useEffect(() => {
    return () => {
      if (refreshTimer.current) clearTimeout(refreshTimer.current);
    };
  }, []);
  useWebSocket('usage_stats_changed', useCallback(() => {
    if (refreshTimer.current) return;
    refreshTimer.current = setTimeout(() => {
      refreshTimer.current = null;
      if (page === 1) fetchConversations(1, true);
    }, 1000);
  }, [page, keyFilter]));

  // Load on page/filter change
  useEffect(() => {
    fetchConversations(page);
  }, [page, keyFilter]);

  // Load service keys for filter dropdown
  useEffect(() => {
    serviceKeysApi.list().then(setServiceKeys).catch(() => {});
  }, []);

  // Auto-scroll detail to bottom when conversation opens.
  // Fallback: schedule a delayed scroll in case onAnimationEnd doesn't fire
  // (e.g., reduced motion or the animation event being swallowed).
  useEffect(() => {
    if (selectedConv) {
      const id = setTimeout(scrollDetailToBottom, 300);
      return () => clearTimeout(id);
    }
  }, [selectedConv, scrollDetailToBottom]);

  // Keyboard navigation
  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      const tag = (e.target as HTMLElement).tagName;
      if (tag === 'INPUT' || tag === 'TEXTAREA' || tag === 'SELECT') return;
      if (e.key === 'ArrowLeft' && page > 1) setPage(p => p - 1);
      if (e.key === 'ArrowRight' && page < totalPages) setPage(p => p + 1);
    };
    window.addEventListener('keydown', handler);
    return () => window.removeEventListener('keydown', handler);
  }, [page, totalPages]);

  return (
    <div className="space-y-6">
      {/* Header — 只有标题 + 密钥筛选 */}
      <div className="flex justify-between items-start gap-4 flex-wrap">
        <h2 className="text-3xl font-normal m-0 select-none" onContextMenu={handleContextMenu}>
          {t('audit.title')}
          {developerMode && <span className="ml-2 text-sm text-muted-foreground">⚡</span>}
        </h2>
        <div className="relative">
          <select
            value={keyFilter}
            onChange={e => { setKeyFilter(e.target.value); setPage(1); }}
            className="h-9 appearance-none rounded-md border border-input bg-background pl-3 pr-8 text-sm focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
          >
            <option value="">{t('common.all')}</option>
            {serviceKeys.map(sk => (
              <option key={sk.id} value={sk.id}>{sk.name} ({sk.key_masked})</option>
            ))}
          </select>
          <ChevronDown className="pointer-events-none absolute right-2.5 top-1/2 h-4 w-4 -translate-y-1/2 text-muted-foreground" />
        </div>
      </div>

      {/* Conversation list */}
      {loading && conversations.length === 0 ? (
        <div className="flex flex-col items-center justify-center py-16">
          <div className="animate-spin rounded-full h-8 w-8 border-b-2 border-primary"></div>
        </div>
      ) : conversations.length === 0 ? (
        <div className="flex flex-col items-center gap-2 py-16 text-center">
          <Inbox className="w-12 h-12 text-muted-foreground" />
          <p className="text-lg">{t('common.empty')}</p>
        </div>
      ) : (
        <div className="rounded-xl border bg-card divide-y divide-border overflow-hidden">
            {conversations.map(conv => (
              <div
                key={conv.id}
                className="w-full text-left px-5 py-4 hover:bg-muted/50 transition-colors cursor-pointer grid grid-cols-[1fr_auto_auto] items-start gap-3"
                onClick={() => fetchDetail(conv.id)}
              >
                <div className="min-w-0 overflow-hidden">
                  <div className="flex items-center gap-2 mb-1 flex-wrap">
                    <span className="text-xs font-medium px-2 py-0.5 rounded-full bg-primary/10 text-primary shrink-0">
                      {conv.service_key_name}
                    </span>
                    <span className="text-xs text-muted-foreground shrink-0">
                      {t('audit.messages', { count: conv.message_count })}
                    </span>
                    <span className="text-xs text-muted-foreground shrink-0">·</span>
                    <span className="text-xs text-muted-foreground shrink-0">
                      {t('audit.requests', { count: conv.request_count })}
                    </span>
                  </div>
                  <p className="text-sm text-foreground/80 truncate">
                    {developerMode
                      ? (conv.last_message_raw || conv.first_user_message || <span className="italic text-muted-foreground">...</span>)
                      : (conv.last_message || conv.first_user_message || <span className="italic text-muted-foreground">...</span>)}
                  </p>
                </div>
                <span className="text-xs text-muted-foreground whitespace-nowrap shrink-0 mt-1">
                  {formatTime(conv.updated_at)}
                </span>
                <div className="shrink-0 mt-0.5" onClick={(e) => e.stopPropagation()}>
                  <DropdownMenu>
                    <DropdownMenuTrigger asChild>
                      <Button
                        variant="ghost"
                        size="icon"
                        className="w-7 h-7 opacity-60 hover:opacity-100"
                        aria-label={t('audit.actions')}
                      >
                        <MoreVertical className="w-4 h-4" />
                      </Button>
                    </DropdownMenuTrigger>
                    <DropdownMenuContent align="end">
                      <DropdownMenuItem onClick={() => handleSaveAsTxtById(conv.id)}>
                        <FileDown className="w-4 h-4 mr-2" />
                        {t('audit.save_as')}
                      </DropdownMenuItem>
                      <DropdownMenuItem
                        onClick={() => handleDeleteClick(conv)}
                        className="text-destructive focus:text-destructive"
                      >
                        <Trash2 className="w-4 h-4 mr-2" />
                        {t('common.delete')}
                      </DropdownMenuItem>
                    </DropdownMenuContent>
                  </DropdownMenu>
                </div>
              </div>
            ))}
          </div>
        )}

      {/* Pagination */}
      {totalPages > 1 && (
        <div className="flex items-center justify-center gap-3">
          <Button
            variant="outline"
            size="sm"
            disabled={page <= 1}
            onClick={() => setPage(p => p - 1)}
          >
            <ChevronLeft className="w-4 h-4" />
            {t('audit.prev')}
          </Button>
          <span className="text-sm text-muted-foreground">
            {t('audit.page', { current: page, total: totalPages })}
          </span>
          <Button
            variant="outline"
            size="sm"
            disabled={page >= totalPages}
            onClick={() => setPage(p => p + 1)}
          >
            {t('audit.next')}
            <ChevronRight className="w-4 h-4" />
          </Button>
        </div>
      )}

      {/* Detail dialog — 无标题、无元信息；右上角三点点菜单 */}
      <Dialog open={!!selectedConv} onOpenChange={(open) => { if (!open) setSelectedConv(null); }}>
        <DialogContent className="max-w-3xl max-h-[80vh]" onAnimationEnd={scrollDetailToBottom}>
          {detailLoading && !selectedConv && (
            <div className="flex justify-center py-8">
              <Loader2 className="w-6 h-6 animate-spin text-muted-foreground" />
            </div>
          )}

          {selectedConv && (
            <div ref={detailRef} className="overflow-y-auto max-h-[calc(80vh-3rem)] space-y-3 mt-2 pr-2">
                {selectedConv.messages.map((msg, idx) => {
                const isUser = msg.role === 'user';
                const text = extractTextFromBlocks(msg.content as unknown[], !developerMode);
                const hasThinking = Array.isArray(msg.content) && (msg.content as any[]).some((b: any) => b?.type === 'thinking');
                const toolUses = Array.isArray(msg.content) ? (msg.content as any[]).filter((b: any) => b?.type === 'tool_use') : [];
                const toolResults = Array.isArray(msg.content) ? (msg.content as any[]).filter((b: any) => b?.type === 'tool_result') : [];
                const images = Array.isArray(msg.content) ? (msg.content as any[]).filter((b: any) => b?.type === 'image') : [];

                // Skip empty messages (after stripping system blocks)
                const hasContent = text || images.length > 0 || (developerMode && (toolUses.length > 0 || toolResults.length > 0 || hasThinking));
                if (!hasContent) return null;

                return (
                  <div key={idx} className={cn('flex gap-3')}>
                    <div className={cn(
                      'flex-shrink-0 w-8 h-8 rounded-full flex items-center justify-center',
                      isUser ? 'bg-primary/10' : 'bg-muted'
                    )}>
                      {isUser
                        ? <User className="w-4 h-4 text-primary" />
                        : <Bot className="w-4 h-4 text-muted-foreground" />
                      }
                    </div>
                    <div className={cn(
                      'flex-1 rounded-lg px-4 py-3 text-sm',
                      isUser ? 'bg-primary/5' : 'bg-muted/50'
                    )}>
                      <div className="text-xs font-medium mb-1.5 text-muted-foreground">
                        {isUser ? t('audit.role_user') : t('audit.role_assistant')}
                      </div>

                      {text && (
                        <p className="whitespace-pre-wrap break-words leading-relaxed">{text}</p>
                      )}

                      {images.length > 0 && (
                        <p className="text-muted-foreground italic text-xs mt-1">
                          {t('audit.image_placeholder')} ×{images.length}
                        </p>
                      )}

                      {/* Thinking blocks — only visible in developer mode */}
                      {developerMode && hasThinking && (
                        <details className="mt-2">
                          <summary className="text-xs text-muted-foreground cursor-pointer hover:text-foreground transition-colors">
                            💭 {t('audit.thinking')}
                          </summary>
                          <pre className="mt-1 text-xs text-muted-foreground whitespace-pre-wrap max-h-40 overflow-y-auto bg-background/50 rounded p-2">
                            {Array.isArray(msg.content)
                              ? (msg.content as any[]).filter((b: any) => b?.type === 'thinking').map((b: any) => b.thinking).join('\n')
                              : ''}
                          </pre>
                        </details>
                      )}

                      {/* Tool blocks — only visible in developer mode */}
                      {developerMode && (
                        <>
                          {toolUses.map((tu: any, i: number) => (
                            <details key={i} className="mt-2">
                              <summary className="text-xs text-muted-foreground cursor-pointer hover:text-foreground transition-colors">
                                🔧 {t('audit.tool_use', { name: tu.name })}
                              </summary>
                              <pre className="mt-1 text-xs whitespace-pre-wrap max-h-40 overflow-y-auto bg-background/50 rounded p-2">
                                {JSON.stringify(tu.input, null, 2)}
                              </pre>
                            </details>
                          ))}

                          {toolResults.map((tr: any, i: number) => (
                            <details key={i} className="mt-2">
                              <summary className="text-xs text-muted-foreground cursor-pointer hover:text-foreground transition-colors">
                                📋 {t('audit.tool_result')} {tr.is_error && '⚠️'}
                              </summary>
                              <pre className="mt-1 text-xs whitespace-pre-wrap max-h-40 overflow-y-auto bg-background/50 rounded p-2">
                                {typeof tr.content === 'string'
                                  ? tr.content
                                  : JSON.stringify(tr.content, null, 2)}
                              </pre>
                            </details>
                          ))}
                        </>
                      )}
                    </div>
                  </div>
                );
              })}
            </div>
          )}
        </DialogContent>
      </Dialog>

      {/* Delete confirmation dialog */}
      <Dialog open={deleteDialogOpen} onOpenChange={setDeleteDialogOpen}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>{t('audit.delete')}</DialogTitle>
            <DialogDescription>
              {t('audit.delete_confirm')}
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button variant="outline" onClick={() => setDeleteDialogOpen(false)} disabled={deleting}>
              {t('common.cancel')}
            </Button>
            <Button variant="destructive" onClick={handleConfirmDelete} disabled={deleting}>
              {deleting ? <Loader2 className="w-4 h-4 animate-spin mr-2" /> : null}
              {t('common.delete')}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      {/* Right-click context menu */}
      {contextMenu && (
        <>
          <div
            className="fixed inset-0 z-40"
            onClick={() => setContextMenu(null)}
            onContextMenu={(e) => { e.preventDefault(); setContextMenu(null); }}
          />
          <div
            className="fixed z-50 min-w-[8rem] overflow-hidden rounded-md border bg-popover p-1 text-popover-foreground shadow-md"
            style={{ left: contextMenu.x, top: contextMenu.y }}
          >
            <button
              className="relative flex w-full cursor-pointer select-none items-center rounded-sm px-2 py-1.5 text-sm outline-none hover:bg-accent hover:text-accent-foreground"
              onClick={handleToggleMode}
            >
              {developerMode ? t('audit.mode_simple') : t('audit.mode_raw')}
            </button>
          </div>
        </>
      )}
    </div>
  );
}

export default AuditView;
