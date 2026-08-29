import { useState, useEffect, useRef } from 'react';
import { useNavigate, useParams } from 'react-router';
import { Search, ArrowLeft, Download, Loader2, Heart, CloudDownload, Inbox } from 'lucide-react';
import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';
import {
  Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle,
} from '@/components/ui/dialog';
import { localModelsApi, type HfRepoSummary, type HfRepoDetail, type HfFile } from '@/lib/api';
import { useLocalModelsStore } from '@/stores/localModels';
import { useT } from '@/i18n';
import { cn } from '@/lib/utils';

function formatSize(bytes: number | null): string {
  if (bytes == null) return '—';
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(0)} KB`;
  if (bytes < 1024 * 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
  return `${(bytes / (1024 * 1024 * 1024)).toFixed(2)} GB`;
}

export function HfBrowseView() {
  const t = useT();
  const navigate = useNavigate();
  const fetchModels = useLocalModelsStore((s) => s.fetchModels);

  const [query, setQuery] = useState('');
  const [searching, setSearching] = useState(false);
  const [results, setResults] = useState<HfRepoSummary[]>([]);
  const [mirror, setMirror] = useState(false);
  const [selectedRepo, setSelectedRepo] = useState<HfRepoDetail | null>(null);
  const [selectedRepoId, setSelectedRepoId] = useState<string | null>(null);
  const [loadingRepo, setLoadingRepo] = useState(false);

  // 创建对话框
  const [createOpen, setCreateOpen] = useState(false);
  const [selectedFile, setSelectedFile] = useState<HfFile | null>(null);
  const [modelId, setModelId] = useState('');
  const [ctxSize, setCtxSize] = useState('32768');
  const [gpuLayers, setGpuLayers] = useState('99');
  const [autostart, setAutostart] = useState(false);
  const [creating, setCreating] = useState(false);
  const [createError, setCreateError] = useState<string | null>(null);

  const searchTimer = useRef<ReturnType<typeof setTimeout> | null>(null);

  const doSearch = async (q: string) => {
    if (!q.trim()) return;
    setSearching(true);
    try {
      const res = await localModelsApi.hfSearch(q, 'gguf', 30);
      setResults(res.results);
      setMirror(res.mirror);
    } catch {
      setResults([]);
    } finally {
      setSearching(false);
    }
  };

  const handleSearchChange = (val: string) => {
    setQuery(val);
    if (searchTimer.current) clearTimeout(searchTimer.current);
    searchTimer.current = setTimeout(() => doSearch(val), 500);
  };

  useEffect(() => {
    return () => { if (searchTimer.current) clearTimeout(searchTimer.current); };
  }, []);

  // 初始搜索
  useEffect(() => {
    doSearch('qwen3');
  }, []);

  const openRepo = async (repo: HfRepoSummary) => {
    // 立即进入详情页，开始加载
    setSelectedRepoId(repo.id);
    setSelectedRepo(null);
    setLoadingRepo(true);
    try {
      const [owner, name] = repo.id.split('/');
      const detail = await localModelsApi.hfRepoDetail(owner, name);
      setSelectedRepo(detail);
    } catch {
      // ignore
    } finally {
      setLoadingRepo(false);
    }
  };

  const openCreate = (file: HfFile) => {
    if (!selectedRepo) return;
    setSelectedFile(file);
    // 默认 model_id：从文件名推断
    const name = file.path.replace(/\.[^.]+$/, '').replace(/[-_]/g, '-').toLowerCase();
    setModelId(name);
    setCreateError(null);
    setCreateOpen(true);
  };

  const handleCreate = async () => {
    if (!selectedRepo || !selectedFile) return;
    setCreating(true);
    setCreateError(null);
    try {
      await localModelsApi.create({
        repo_id: selectedRepo.id,
        filename: selectedFile.path,
        format: 'gguf',
        backend: 'auto',
        model_id: modelId,
        ctx_size: parseInt(ctxSize) || 32768,
        n_gpu_layers: parseInt(gpuLayers) || 99,
        autostart,
      });
      setCreateOpen(false);
      await fetchModels();
      navigate('/local');
    } catch (e: any) {
      setCreateError(e.message || '创建失败');
    } finally {
      setCreating(false);
    }
  };

  const toggleMirror = async () => {
    const newMirror = !mirror;
    try {
      await localModelsApi.hfMirror(newMirror);
      setMirror(newMirror);
    } catch { /* ignore */ }
  };

  // 过滤 GGUF 文件
  const ggufFiles = selectedRepo?.files.filter((f) =>
    f.path.endsWith('.gguf') && !f.path.includes('/')
  ) ?? [];

  return (
    <div className="space-y-6">
      <div className="flex items-center gap-3">
        <Button
          variant="ghost"
          size="icon"
          onClick={() => selectedRepoId ? setSelectedRepoId(null) : navigate('/local')}
        >
          <ArrowLeft className="w-5 h-5" />
        </Button>
        <h2 className="text-3xl font-normal m-0 truncate">
          {selectedRepoId || t('local.browse_hf')}
        </h2>
      </div>

      {/* 搜索栏（仅列表视图显示） */}
      {!selectedRepoId && (
        <div className="flex items-center gap-2">
          <div className="relative flex-1">
            <Search className="absolute left-3 top-1/2 -translate-y-1/2 w-4 h-4 text-muted-foreground" />
            <Input
              className="pl-9"
              placeholder={t('local.hf_search_placeholder')}
              value={query}
              onChange={(e) => handleSearchChange(e.target.value)}
            />
          </div>
          <Button variant="outline" size="sm" onClick={toggleMirror}>
            {t('local.hf_mirror')}：{mirror ? 'ON' : 'OFF'}
          </Button>
        </div>
      )}

      {/* 详情页 */}
      {selectedRepoId ? (
        loadingRepo ? (
          <div className="flex items-center justify-center py-16">
            <Loader2 className="w-8 h-8 animate-spin text-muted-foreground" />
          </div>
        ) : selectedRepo ? (
          <div className="space-y-4">
            {selectedRepo.description && (
              <p className="text-sm text-muted-foreground">{selectedRepo.description}</p>
            )}
            <div className="flex gap-4 text-sm text-muted-foreground">
              <span className="inline-flex items-center gap-1"><Heart className="w-4 h-4" /> {selectedRepo.likes}</span>
              <span className="inline-flex items-center gap-1"><Download className="w-4 h-4" /> {selectedRepo.downloads.toLocaleString()}</span>
            </div>
            {ggufFiles.length === 0 ? (
              <p className="text-muted-foreground py-8 text-center">{t('local.hf_no_gguf')}</p>
            ) : (
              <div className="space-y-2">
                {ggufFiles.map((f) => (
                  <div key={f.path} className="flex items-center justify-between bg-muted rounded-lg p-3">
                    <div className="min-w-0 flex-1">
                      <p className="font-mono text-sm truncate" title={f.path}>{f.path}</p>
                      <p className="text-xs text-muted-foreground">{formatSize(f.size)}</p>
                    </div>
                    <Button size="sm" onClick={() => openCreate(f)}>
                      <Download className="w-4 h-4 mr-1" />
                      {t('local.hf_download')}
                    </Button>
                  </div>
                ))}
              </div>
            )}
          </div>
        ) : null
      ) : (
        /* 搜索结果列表 */
        <div className="space-y-2">
          {searching ? (
            <div className="flex items-center justify-center py-16">
              <Loader2 className="w-8 h-8 animate-spin text-muted-foreground" />
            </div>
          ) : results.length === 0 ? (
            <div className="flex flex-col items-center gap-2 py-16 text-center">
              <CloudDownload className="w-12 h-12 text-muted-foreground" />
              <p className="text-muted-foreground">{t('local.hf_no_results')}</p>
            </div>
          ) : (
            results.map((repo) => (
              <div
                key={repo.id}
                className="w-full text-left bg-muted rounded-lg p-4"
              >
                <div className="flex items-start justify-between gap-3">
                  <div className="min-w-0 flex-1">
                    <h4 className="font-medium truncate">{repo.id}</h4>
                    {repo.description && (
                      <p className="text-sm text-muted-foreground mt-1 line-clamp-2">{repo.description}</p>
                    )}
                    <div className="flex gap-3 mt-2 text-xs text-muted-foreground">
                      <span className="inline-flex items-center gap-1"><Heart className="w-3.5 h-3.5" /> {repo.likes}</span>
                      <span className="inline-flex items-center gap-1"><Download className="w-3.5 h-3.5" /> {repo.downloads.toLocaleString()}</span>
                    </div>
                  </div>
                  <Button variant="outline" size="sm" className="shrink-0" onClick={() => openRepo(repo)}>
                    {t('local.hf_weights')}
                  </Button>
                </div>
              </div>
            ))
          )}
        </div>
      )}

      {/* 创建下载对话框 */}
      <Dialog open={createOpen} onOpenChange={setCreateOpen}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>{t('local.create_title')}</DialogTitle>
            <DialogDescription>
              {selectedRepo && selectedFile && (
                <span className="font-mono text-xs">{selectedRepo.id}/{selectedFile.path}</span>
              )}
            </DialogDescription>
          </DialogHeader>
          <div className="space-y-4 py-2">
            <div className="space-y-2">
              <label className="text-sm font-medium">{t('local.model_id_label')}</label>
              <Input value={modelId} onChange={(e) => setModelId(e.target.value)} placeholder="my-local-model" />
              <p className="text-xs text-muted-foreground">{t('local.model_id_hint')}</p>
            </div>
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
            <label className="flex items-center gap-2 text-sm cursor-pointer">
              <input type="checkbox" checked={autostart} onChange={(e) => setAutostart(e.target.checked)} className="rounded" />
              {t('local.autostart_label')}
            </label>
            {createError && <p className="text-sm text-destructive">{createError}</p>}
          </div>
          <DialogFooter>
            <Button variant="outline" onClick={() => setCreateOpen(false)}>{t('common.cancel')}</Button>
            <Button onClick={handleCreate} disabled={creating || !modelId.trim()}>
              {creating && <Loader2 className="w-4 h-4 mr-2 animate-spin" />}
              {t('local.create_btn')}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}

export default HfBrowseView;
