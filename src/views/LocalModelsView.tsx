import { useState, useEffect, useCallback } from 'react';
import { useNavigate } from 'react-router';
import {
  CloudDownload, FolderOpen, Plus, Play, Square, Pencil, Trash2, Inbox,
  XCircle, Loader2,
  MoreVertical,
} from 'lucide-react';
import { Button } from '@/components/ui/button';
import {
  Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle,
} from '@/components/ui/dialog';
import {
  DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu';
import { Input } from '@/components/ui/input';
import { useLocalModelsStore } from '@/stores/localModels';
import { localModelsApi, type LocalModel } from '@/lib/api';
import { useWebSocket } from '@/hooks/useWebSocket';
import { useT } from '@/i18n';
import { cn } from '@/lib/utils';
import { tauriDialog, isTauri } from '@/lib/tauri';

const STATUS_COLORS: Record<string, string> = {
  downloading: 'bg-blue-500',
  downloaded: 'bg-gray-400',
  running: 'bg-green-500',
  error: 'bg-red-500',
};

function backendLabel(backend: string) {
  const labels: Record<string, string> = {
    auto: 'Auto', cpu: 'CPU', cuda: 'CUDA', vulkan: 'Vulkan', rocm: 'ROCm', metal: 'Metal',
  };
  return labels[backend] || backend;
}

function formatSize(bytes: number | null): string {
  if (bytes == null) return '—';
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(0)} KB`;
  if (bytes < 1024 * 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
  return `${(bytes / (1024 * 1024 * 1024)).toFixed(2)} GB`;
}

const BACKEND_OPTIONS = [
  { value: 'auto', label: 'Auto' },
  { value: 'metal', label: 'Metal' },
  { value: 'cuda', label: 'CUDA' },
  { value: 'vulkan', label: 'Vulkan' },
  { value: 'rocm', label: 'ROCm' },
  { value: 'cpu', label: 'CPU' },
];

function ModelCard({ model }: { model: LocalModel }) {
  const t = useT();
  const progress = useLocalModelsStore((s) => s.progress[model.id]);
  const [editing, setEditing] = useState(false);
  const [deleting, setDeleting] = useState(false);
  const [modelId, setModelId] = useState(model.model_id);
  const [ctxSize, setCtxSize] = useState(model.ctx_size.toString());
  const [gpuLayers, setGpuLayers] = useState(model.n_gpu_layers.toString());
  const [backend, setBackend] = useState(model.backend);
  const [thinking, setThinking] = useState(model.thinking === 1);
  const [autostart, setAutostart] = useState(model.autostart === 1);
  const [busy, setBusy] = useState(false);
  const { fetchModels } = useLocalModelsStore();

  const progressPct = progress && progress.total > 0
    ? Math.min(100, (progress.downloaded / progress.total) * 100)
    : 0;

  const handleStart = async () => { setBusy(true); try { await localModelsApi.start(model.id); } finally { setBusy(false); } };
  const handleStop = async () => { setBusy(true); try { await localModelsApi.stop(model.id); } finally { setBusy(false); } };
  const handleCancel = async () => { try { await localModelsApi.cancel(model.id); } catch { /* ignore */ } };

  const handleEditSave = async () => {
    setBusy(true);
    try {
      await localModelsApi.edit(model.id, {
        model_id: modelId || model.model_id,
        ctx_size: parseInt(ctxSize) || model.ctx_size,
        n_gpu_layers: parseInt(gpuLayers) || model.n_gpu_layers,
        backend,
        thinking,
        autostart,
      });
      await fetchModels();
      setEditing(false);
    } finally { setBusy(false); }
  };

  const handleDelete = async () => {
    setBusy(true);
    try {
      await localModelsApi.delete(model.id, true);
      await fetchModels();
    } finally {
      setBusy(false);
      setDeleting(false);
    }
  };

  return (
    <article className="bg-muted rounded-lg p-5 space-y-3">
      <div className="flex items-start gap-3">
        <div className="flex-1 min-w-0">
          <div className="flex items-center gap-2 min-w-0">
            <h3 className="font-medium truncate" title={model.model_id}>{model.model_id}</h3>
            <span className={cn('inline-flex items-center gap-1 text-xs px-2 py-0.5 rounded-full text-white shrink-0', STATUS_COLORS[model.status])}>
              {model.status === 'downloading' && <Loader2 className="w-3 h-3 animate-spin" />}
              {t(`local.status.${model.status}`)}
            </span>
          </div>
          <p className="text-xs text-muted-foreground font-mono truncate mt-0.5" title={`${model.repo_id}/${model.filename}`}>
            {model.repo_id}/{model.filename}
          </p>
        </div>
        <DropdownMenu>
          <DropdownMenuTrigger asChild>
            <Button variant="ghost" size="icon" className="w-9 h-9 shrink-0" disabled={busy}>
              {busy ? <Loader2 className="w-4 h-4 animate-spin" /> : <MoreVertical className="w-5 h-5" />}
            </Button>
          </DropdownMenuTrigger>
          <DropdownMenuContent align="end">
            {model.status === 'downloading' ? (
              <DropdownMenuItem onClick={handleCancel} className="text-destructive focus:text-destructive">
                <XCircle className="w-4 h-4 mr-2" />
                {t('local.actions.cancel')}
              </DropdownMenuItem>
            ) : (
              <>
                {model.status !== 'running' ? (
                  <DropdownMenuItem onClick={handleStart} disabled={model.status === 'downloading'}>
                    <Play className="w-4 h-4 mr-2" />
                    {t('local.actions.start')}
                  </DropdownMenuItem>
                ) : (
                  <DropdownMenuItem onClick={handleStop}>
                    <Square className="w-4 h-4 mr-2" />
                    {t('local.actions.stop')}
                  </DropdownMenuItem>
                )}
                <DropdownMenuItem onClick={() => {
                  setModelId(model.model_id);
                  setBackend(model.backend);
                  setCtxSize(model.ctx_size.toString());
                  setGpuLayers(model.n_gpu_layers.toString());
                  setThinking(model.thinking === 1);
                  setAutostart(model.autostart === 1);
                  setEditing(true);
                }}>
                  <Pencil className="w-4 h-4 mr-2" />
                  {t('local.actions.edit')}
                </DropdownMenuItem>
                <DropdownMenuItem onClick={() => setDeleting(true)} className="text-destructive focus:text-destructive">
                  <Trash2 className="w-4 h-4 mr-2" />
                  {t('local.actions.delete')}
                </DropdownMenuItem>
              </>
            )}
          </DropdownMenuContent>
        </DropdownMenu>
      </div>

      {model.status === 'downloading' && progress && (
        <div className="space-y-1">
          <div className="w-full h-2 bg-background rounded-full overflow-hidden">
            <div className="h-full bg-blue-500 transition-all" style={{ width: `${progressPct}%` }} />
          </div>
          <p className="text-xs text-muted-foreground">
            {formatSize(progress.downloaded)} / {formatSize(progress.total)} ({progressPct.toFixed(1)}%)
          </p>
        </div>
      )}

      <div className="text-xs text-muted-foreground">
        {formatSize(model.file_size)}, {backendLabel(model.backend)}, {Math.round(model.ctx_size / 1024)}k, {model.n_gpu_layers} layers
      </div>

      {/* Edit Dialog */}
      <Dialog open={editing} onOpenChange={setEditing}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>{t('local.edit_title')}</DialogTitle>
            <DialogDescription>{t('local.edit_desc')}</DialogDescription>
          </DialogHeader>
          <div className="space-y-4 py-2">
            <div className="space-y-2">
              <label className="text-sm font-medium">{t('local.model_id_label')}</label>
              <Input value={modelId} onChange={(e) => setModelId(e.target.value)} />
            </div>
            <div className="space-y-2">
              <label className="text-sm font-medium">{t('local.backend')}</label>
              <select
                className="flex h-9 w-full rounded-md border border-input bg-background px-3 py-1 text-sm shadow-sm"
                value={backend}
                onChange={(e) => setBackend(e.target.value)}
              >
                {BACKEND_OPTIONS.map((opt) => (
                  <option key={opt.value} value={opt.value}>{opt.label}</option>
                ))}
              </select>
            </div>
            <label className="flex items-center gap-2 text-sm cursor-pointer">
              <input type="checkbox" checked={thinking} onChange={(e) => setThinking(e.target.checked)} className="rounded" />
              {t('local.thinking_label')}
            </label>
            <label className="flex items-center gap-2 text-sm cursor-pointer">
              <input type="checkbox" checked={autostart} onChange={(e) => setAutostart(e.target.checked)} className="rounded" />
              {t('local.autostart_label')}
            </label>
            <div className="grid grid-cols-2 gap-3">
              <div className="space-y-2">
                <label className="text-sm font-medium">{t('local.ctx_size')}</label>
                <Input type="number" value={ctxSize} onChange={(e) => setCtxSize(e.target.value)} />
              </div>
              <div className="space-y-2">
                <label className="text-sm font-medium">{t('local.n_gpu_layers')}</label>
                <Input type="number" value={gpuLayers} onChange={(e) => setGpuLayers(e.target.value)} />
              </div>
            </div>
          </div>
          <DialogFooter>
            <Button variant="outline" onClick={() => setEditing(false)}>{t('common.cancel')}</Button>
            <Button onClick={handleEditSave} disabled={busy}>
              {busy && <Loader2 className="w-4 h-4 mr-2 animate-spin" />}
              {t('common.save')}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      {/* Delete Dialog */}
      <Dialog open={deleting} onOpenChange={setDeleting}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>{t('local.delete_title')}</DialogTitle>
            <DialogDescription>
              {t('local.delete_confirm', { name: model.model_id })}
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button variant="outline" onClick={() => setDeleting(false)}>{t('common.cancel')}</Button>
            <Button variant="destructive" onClick={handleDelete} disabled={busy}>
              {busy && <Loader2 className="w-4 h-4 mr-2 animate-spin" />}
              {t('local.delete_confirm_btn')}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </article>
  );
}

export function LocalModelsView() {
  const t = useT();
  const navigate = useNavigate();
  const { models, loading, fetchModels } = useLocalModelsStore();
  const updateProgress = useLocalModelsStore((s) => s.updateProgress);
  const updateStatus = useLocalModelsStore((s) => s.updateStatus);

  // 添加权重：来源选择 + 本地导入表单
  const [addOpen, setAddOpen] = useState(false);
  const [importOpen, setImportOpen] = useState(false);
  const [importPath, setImportPath] = useState('');
  const [importModelId, setImportModelId] = useState('');
  const [importCtxSize, setImportCtxSize] = useState('32768');
  const [importGpuLayers, setImportGpuLayers] = useState('99');
  const [importAutostart, setImportAutostart] = useState(false);
  const [importing, setImporting] = useState(false);
  const [importError, setImportError] = useState<string | null>(null);

  const openImport = () => {
    setAddOpen(false);
    setImportPath('');
    setImportModelId('');
    setImportCtxSize('32768');
    setImportGpuLayers('99');
    setImportAutostart(false);
    // 浏览器环境没有原生文件选择，本地导入仅桌面端可用
    setImportError(isTauri() ? null : t('local.import_browser_unsupported'));
    setImportOpen(true);
  };

  const pickImportFile = async () => {
    const path = await tauriDialog.open({
      title: t('local.import_dialog_title'),
      filters: [{ name: 'GGUF', extensions: ['gguf'] }],
    });
    if (!path) return;
    setImportPath(path);
    if (!importModelId.trim()) {
      // 默认别名：从文件名推断（与 HF 下载一致）
      const name = path
        .split(/[\\/]/)
        .pop()!
        .replace(/\.[^.]+$/, '')
        .replace(/[-_]/g, '-')
        .toLowerCase();
      setImportModelId(name);
    }
  };

  const handleImport = async () => {
    if (!importPath) return;
    setImporting(true);
    setImportError(null);
    try {
      await localModelsApi.create({
        repo_id: 'local',
        filename: importPath.split(/[\\/]/).pop() || 'model.gguf',
        format: 'gguf',
        backend: 'auto',
        model_id: importModelId,
        ctx_size: parseInt(importCtxSize) || 32768,
        n_gpu_layers: parseInt(importGpuLayers) || 99,
        autostart: importAutostart,
        local_path: importPath,
      });
      setImportOpen(false);
      await fetchModels();
    } catch (e: any) {
      setImportError(e.message || '导入失败');
    } finally {
      setImporting(false);
    }
  };

  useEffect(() => {
    fetchModels();
  }, [fetchModels]);

  // WebSocket: local_progress / local_status 事件
  const handleWs = useCallback((data: any) => {
    if (data?.type === 'local_progress') {
      updateProgress(data.id, data.downloaded, data.total);
    } else if (data?.type === 'local_status') {
      updateStatus(data.id, data.status, data.port);
      fetchModels();
    }
  }, [updateProgress, updateStatus, fetchModels]);
  useWebSocket('local_progress', handleWs);
  useWebSocket('local_status', handleWs);

  return (
    <div className="flex flex-col h-full">
      <div className="flex-1 overflow-auto">
        <div className="space-y-6">
          <div className="flex justify-between items-start gap-4 flex-wrap">
            <h2 className="text-3xl font-normal m-0">{t('local.title')}</h2>
            <div className="flex items-center gap-2">
              <Button onClick={() => setAddOpen(true)}>
                <Plus className="w-4 h-4 mr-2" />
                {t('local.add')}
              </Button>
            </div>
          </div>

          {loading && models.length === 0 ? (
            <div className="flex flex-col items-center justify-center py-16">
              <Loader2 className="w-8 h-8 animate-spin text-muted-foreground" />
            </div>
          ) : models.length === 0 ? (
            <div className="flex flex-col items-center gap-2 py-16 text-center">
              <Inbox className="w-12 h-12 text-muted-foreground" />
              <p className="text-lg">{t('common.empty')}</p>
            </div>
          ) : (
            <div className="grid grid-cols-[repeat(auto-fill,minmax(340px,1fr))] gap-4">
              {models.map((m) => (
                <ModelCard key={m.id} model={m} />
              ))}
            </div>
          )}
        </div>
      </div>

      {/* 添加权重：来源选择 */}
      <Dialog open={addOpen} onOpenChange={setAddOpen}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>{t('local.add')}</DialogTitle>
            <DialogDescription>{t('local.add_dialog_desc')}</DialogDescription>
          </DialogHeader>
          <div className="grid grid-cols-2 gap-3 py-2">
            <button
              className="flex flex-col items-center gap-2 rounded-lg border border-border bg-background p-5 text-center hover:bg-accent transition-colors cursor-pointer"
              onClick={openImport}
            >
              <FolderOpen className="w-8 h-8 text-muted-foreground" />
              <span className="text-sm font-medium">{t('local.import_local')}</span>
              <span className="text-xs text-muted-foreground">{t('local.import_local_desc')}</span>
            </button>
            <button
              className="flex flex-col items-center gap-2 rounded-lg border border-border bg-background p-5 text-center hover:bg-accent transition-colors cursor-pointer"
              onClick={() => { setAddOpen(false); navigate('/local/hf'); }}
            >
              <CloudDownload className="w-8 h-8 text-muted-foreground" />
              <span className="text-sm font-medium">{t('local.browse_hf')}</span>
              <span className="text-xs text-muted-foreground">{t('local.hf_subtitle')}</span>
            </button>
          </div>
        </DialogContent>
      </Dialog>

      {/* 导入本地权重 */}
      <Dialog open={importOpen} onOpenChange={setImportOpen}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>{t('local.import_dialog_title')}</DialogTitle>
            <DialogDescription>{t('local.import_local_desc')}</DialogDescription>
          </DialogHeader>
          <div className="space-y-4 py-2">
            <div className="space-y-2">
              <label className="text-sm font-medium">{t('local.import_src')}</label>
              <div className="flex gap-2">
                <div
                  className="flex h-9 flex-1 min-w-0 items-center rounded-md border border-input bg-background px-3 text-sm text-muted-foreground truncate cursor-pointer"
                  onClick={pickImportFile}
                  title={importPath}
                >
                  {importPath || t('local.import_src_hint')}
                </div>
                <Button variant="outline" onClick={pickImportFile}>…</Button>
              </div>
            </div>
            <div className="space-y-2">
              <label className="text-sm font-medium">{t('local.model_id_label')}</label>
              <Input value={importModelId} onChange={(e) => setImportModelId(e.target.value)} placeholder="my-local-model" />
              <p className="text-xs text-muted-foreground">{t('local.model_id_hint')}</p>
            </div>
            <div className="grid grid-cols-2 gap-3">
              <div className="space-y-2">
                <label className="text-sm font-medium">{t('local.ctx_size')}</label>
                <Input type="number" value={importCtxSize} onChange={(e) => setImportCtxSize(e.target.value)} />
              </div>
              <div className="space-y-2">
                <label className="text-sm font-medium">{t('local.n_gpu_layers')}</label>
                <Input type="number" value={importGpuLayers} onChange={(e) => setImportGpuLayers(e.target.value)} />
              </div>
            </div>
            <label className="flex items-center gap-2 text-sm cursor-pointer">
              <input type="checkbox" checked={importAutostart} onChange={(e) => setImportAutostart(e.target.checked)} className="rounded" />
              {t('local.autostart_label')}
            </label>
            {importError && <p className="text-sm text-destructive">{importError}</p>}
          </div>
          <DialogFooter>
            <Button variant="outline" onClick={() => setImportOpen(false)}>{t('common.cancel')}</Button>
            <Button onClick={handleImport} disabled={importing || !importModelId.trim()}>
              {importing && <Loader2 className="w-4 h-4 mr-2 animate-spin" />}
              {t('local.import_btn')}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}

export default LocalModelsView;
