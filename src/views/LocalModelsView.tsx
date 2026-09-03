import { useState, useEffect, useCallback } from 'react';
import {
  Plus, Play, Square, Pencil, Trash2, Inbox,
  Loader2,
  MoreVertical,
} from 'lucide-react';
import { Button } from '@/components/ui/button';
import { Checkbox } from '@/components/ui/checkbox';
import { Label } from '@/components/ui/label';
import {
  Select, SelectContent, SelectItem, SelectTrigger, SelectValue,
} from '@/components/ui/select';
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
  downloaded: 'bg-gray-400',
  running: 'bg-green-500',
  error: 'bg-red-500',
};

function backendLabel(backend: string, t: (key: string) => string) {
  const labelKeys: Record<string, string> = {
    auto: 'local.backend.auto', cpu: 'local.backend.cpu', cuda: 'local.backend.cuda',
    vulkan: 'local.backend.vulkan', rocm: 'local.backend.rocm', metal: 'local.backend.metal',
  };
  const key = labelKeys[backend];
  return key ? t(key) : backend;
}

function formatSize(bytes: number | null): string {
  if (bytes == null) return '—';
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(0)} KB`;
  if (bytes < 1024 * 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
  return `${(bytes / (1024 * 1024 * 1024)).toFixed(2)} GB`;
}

const BACKEND_OPTIONS = [
  { value: 'auto', labelKey: 'local.backend.auto' },
  { value: 'metal', labelKey: 'local.backend.metal' },
  { value: 'cuda', labelKey: 'local.backend.cuda' },
  { value: 'vulkan', labelKey: 'local.backend.vulkan' },
  { value: 'rocm', labelKey: 'local.backend.rocm' },
  { value: 'cpu', labelKey: 'local.backend.cpu' },
];

function ModelCard({ model }: { model: LocalModel }) {
  const t = useT();
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

  const handleStart = async () => { setBusy(true); try { await localModelsApi.start(model.id); } finally { setBusy(false); } };
  const handleStop = async () => { setBusy(true); try { await localModelsApi.stop(model.id); } finally { setBusy(false); } };

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
            {model.status !== 'running' ? (
              <DropdownMenuItem onClick={handleStart}>
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
          </DropdownMenuContent>
        </DropdownMenu>
      </div>

      <div className="text-xs text-muted-foreground">
        {formatSize(model.file_size)}, {backendLabel(model.backend, t)}, {Math.round(model.ctx_size / 1024)}k, {model.n_gpu_layers} layers
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
              <Label>{t('local.model_id_label')}</Label>
              <Input value={modelId} onChange={(e) => setModelId(e.target.value)} />
            </div>
            <div className="space-y-2">
              <Label>{t('local.backend')}</Label>
              <Select value={backend} onValueChange={(v) => setBackend(v)}>
                <SelectTrigger>
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  {BACKEND_OPTIONS.map((opt) => (
                    <SelectItem key={opt.value} value={opt.value}>{t(opt.labelKey)}</SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </div>
            <label className="flex items-center gap-2 text-sm cursor-pointer">
              <Checkbox checked={thinking} onCheckedChange={(v) => setThinking(!!v)} />
              {t('local.thinking_label')}
            </label>
            <label className="flex items-center gap-2 text-sm cursor-pointer">
              <Checkbox checked={autostart} onCheckedChange={(v) => setAutostart(!!v)} />
              {t('local.autostart_label')}
            </label>
            <div className="grid grid-cols-2 gap-3">
              <div className="space-y-2">
                <Label>{t('local.ctx_size')}</Label>
                <Input type="number" value={ctxSize} onChange={(e) => setCtxSize(e.target.value)} />
              </div>
              <div className="space-y-2">
                <Label>{t('local.n_gpu_layers')}</Label>
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
  const { models, loading, fetchModels } = useLocalModelsStore();
  const updateStatus = useLocalModelsStore((s) => s.updateStatus);

  // 导入权重表单
  const [importOpen, setImportOpen] = useState(false);
  const [importPath, setImportPath] = useState('');
  const [importModelId, setImportModelId] = useState('');
  const [importCtxSize, setImportCtxSize] = useState('32768');
  const [importGpuLayers, setImportGpuLayers] = useState('99');
  const [importAutostart, setImportAutostart] = useState(false);
  const [importing, setImporting] = useState(false);
  const [importError, setImportError] = useState<string | null>(null);

  const openImport = () => {
    setImportPath('');
    setImportModelId('');
    setImportCtxSize('32768');
    setImportGpuLayers('99');
    setImportAutostart(false);
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

  // WebSocket: local_status 事件
  const handleWs = useCallback((data: any) => {
    if (data?.type === 'local_status') {
      updateStatus(data.id, data.status, data.port);
      fetchModels();
    }
  }, [updateStatus, fetchModels]);
  useWebSocket('local_status', handleWs);

  return (
    <div className="flex flex-col h-full">
      <div className="flex-1 overflow-auto">
        <div className="space-y-6">
          <div className="flex justify-between items-start gap-4 flex-wrap">
            <h2 className="text-3xl font-normal m-0">{t('local.title')}</h2>
            <div className="flex items-center gap-2">
              <Button onClick={openImport}>
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

      {/* 导入本地权重 */}
      <Dialog open={importOpen} onOpenChange={setImportOpen}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>{t('local.import_dialog_title')}</DialogTitle>
            <DialogDescription>{t('local.import_local_desc')}</DialogDescription>
          </DialogHeader>
          <div className="space-y-4 py-2">
            <div className="space-y-2">
              <Label>{t('local.import_src')}</Label>
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
              <Label>{t('local.model_id_label')}</Label>
              <Input value={importModelId} onChange={(e) => setImportModelId(e.target.value)} placeholder={t('local.model_id_placeholder')} />
              <p className="text-xs text-muted-foreground">{t('local.model_id_hint')}</p>
            </div>
            <div className="grid grid-cols-2 gap-3">
              <div className="space-y-2">
                <Label>{t('local.ctx_size')}</Label>
                <Input type="number" value={importCtxSize} onChange={(e) => setImportCtxSize(e.target.value)} />
              </div>
              <div className="space-y-2">
                <Label>{t('local.n_gpu_layers')}</Label>
                <Input type="number" value={importGpuLayers} onChange={(e) => setImportGpuLayers(e.target.value)} />
              </div>
            </div>
            <label className="flex items-center gap-2 text-sm cursor-pointer">
              <Checkbox checked={importAutostart} onCheckedChange={(v) => setImportAutostart(!!v)} />
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
