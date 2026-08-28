import { useState, useEffect, useMemo, useCallback, useRef } from 'react';
import { useNavigate } from 'react-router';
import { Plus, Inbox, GripVertical, MoreVertical, Pencil, Trash2, ArrowUpDown, Check, Loader2 } from 'lucide-react';
import {
  DndContext,
  closestCenter,
  KeyboardSensor,
  PointerSensor,
  useSensor,
  useSensors,
  DragEndEvent,
} from '@dnd-kit/core';
import {
  arrayMove,
  SortableContext,
  sortableKeyboardCoordinates,
  useSortable,
  rectSortingStrategy,
} from '@dnd-kit/sortable';
import { CSS } from '@dnd-kit/utilities';
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
import { useProvidersStore } from '@/stores/providers';
import { useKeysStore } from '@/stores/keys';
import { useModelsStore } from '@/stores/models';
import { providersApi, pluginsApi, type Provider, type Model, type ApiKey } from '@/lib/api';
import { useWebSocket } from '@/hooks/useWebSocket';
import { useT } from '@/i18n';
import { cn } from '@/lib/utils';
import { listen as tauriListen, isTauri } from '@/lib/tauri';

/** 密钥健康状态颜色（与密钥页 STATUS_COLORS 一致） */
const KEY_STATUS_COLORS: Record<string, string> = {
  green: 'bg-green-500',
  yellow: 'bg-yellow-500',
  red: 'bg-red-500',
  unknown: 'bg-gray-400',
};

/** 提取 Base URL 域名；解析失败则退化为剥掉协议后的首段。 */
function domainOf(url: string): string {
  try {
    return new URL(url).hostname;
  } catch {
    return url.replace(/^[a-z][a-z0-9+.-]*:\/\//i, '').split('/')[0];
  }
}

interface ProviderCardProps {
  provider: Provider;
  models: Model[];
  keys: ApiKey[];
  isPlugin?: boolean;
  pluginOnline?: boolean;
  sortMode: boolean;
  onEdit: () => void;
  onDelete: () => void;
}

function ProviderCard({
  provider,
  models,
  keys,
  isPlugin,
  pluginOnline,
  sortMode,
  onEdit,
  onDelete,
}: ProviderCardProps) {
  const t = useT();
  const { attributes, listeners, setNodeRef, transform, transition, isDragging } = useSortable({
    id: provider.id,
    disabled: !sortMode,
  });

  const style = {
    transform: CSS.Transform.toString(transform),
    transition,
  };

  return (
    <article
      ref={setNodeRef}
      style={style}
      className={cn(
        'bg-muted rounded-lg p-5 grid gap-3 items-start cursor-default',
        // 排序手柄仅在排序模式时显示并占宽，默认隐藏
        sortMode ? 'grid-cols-[24px_1fr_auto]' : 'grid-cols-[1fr_auto]',
        isDragging && 'opacity-50'
      )}
    >
      {sortMode && (
        <div
          {...attributes}
          {...listeners}
          title={t('providers.drag_tip')}
          className="flex items-center justify-center text-muted-foreground cursor-grab text-xl pt-2 touch-none active:cursor-grabbing"
        >
          <GripVertical className="w-5 h-5" />
        </div>
      )}

      <div className="flex flex-col gap-1.5 min-w-0">
        {/* 名称 + 灰度域名（插件供应商显示在线状态） */}
        <div className="flex items-center gap-2 min-w-0">
          <h3 className="font-medium truncate min-w-0" title={provider.name}>
            {provider.name}
          </h3>
          {isPlugin ? (
            <span
              className={cn(
                'text-xs shrink-0',
                // 在线：常规样式（不斜体）；离线：淡化 + 斜体提示，不另标文字
                pluginOnline
                  ? 'text-muted-foreground'
                  : 'text-muted-foreground/50 italic'
              )}
              title={t('providers.plugin_delegated')}
            >
              {t('providers.plugin_badge')}
            </span>
          ) : provider.base_url ? (
            <span
              className="text-xs text-muted-foreground font-mono truncate shrink-0"
              title={provider.base_url}
            >
              {domainOf(provider.base_url)}
            </span>
          ) : null}
        </div>

        {/* 可用模型 chips（样式与「组合」页成员一致），按可用宽度自动换行 */}
        {models.length > 0 && (
          <div className="flex flex-wrap gap-1">
            {models.map((m) => (
              <span
                key={m.id}
                className="inline-flex items-center px-2 py-1 rounded-md bg-background border text-xs"
              >
                <span className="font-mono">{m.display_name || m.model_id}</span>
              </span>
            ))}
          </div>
        )}

        {/* 密钥可用性：每密钥一个小方块，按密钥列表顺序一一对应，宽度不够自动换行 */}
        {keys.length > 0 && (
          <div className="flex flex-wrap gap-1.5 mt-1.5">
            {keys.map((k) => (
              <span
                key={k.id}
                className={cn(
                  'h-3 w-3 rounded-sm shrink-0',
                  KEY_STATUS_COLORS[k.status] || KEY_STATUS_COLORS.unknown
                )}
                title={`${k.name} · ${k.status}`}
              />
            ))}
          </div>
        )}
      </div>

      <DropdownMenu>
        <DropdownMenuTrigger asChild>
          <Button variant="ghost" size="icon" className="w-9 h-9">
            <MoreVertical className="w-5 h-5" />
          </Button>
        </DropdownMenuTrigger>
        <DropdownMenuContent align="end">
          <DropdownMenuItem onClick={onEdit}>
            <Pencil className="w-4 h-4 mr-2" />
            {t('common.edit')}
          </DropdownMenuItem>
          <DropdownMenuItem onClick={onDelete} className="text-destructive focus:text-destructive">
            <Trash2 className="w-4 h-4 mr-2" />
            {t('common.delete')}
          </DropdownMenuItem>
        </DropdownMenuContent>
      </DropdownMenu>
    </article>
  );
}

export function ProvidersView() {
  const t = useT();
  const navigate = useNavigate();
  const { providers, fetchProviders, reorderProviders } = useProvidersStore();
  const { keys, fetchKeys } = useKeysStore();
  const { models, fetchModels } = useModelsStore();

  const [loading, setLoading] = useState(true);
  const [deleteDialogOpen, setDeleteDialogOpen] = useState(false);
  const [deleteTarget, setDeleteTarget] = useState<Provider | null>(null);
  const [pluginOnlineMap, setPluginOnlineMap] = useState<Record<string, boolean>>({});
  // 排序模式：进入后显示拖拽手柄、拖动只改本地顺序，点「保存」才落库
  const [sortMode, setSortMode] = useState(false);
  const [savingSort, setSavingSort] = useState(false);
  const [sortError, setSortError] = useState<string | null>(null);

  // 按供应商分组：模型 chips 与密钥方块
  const modelsByProvider = useMemo(() => {
    const map = new Map<string, Model[]>();
    for (const m of models) {
      const list = map.get(m.provider_id);
      if (list) list.push(m);
      else map.set(m.provider_id, [m]);
    }
    return map;
  }, [models]);

  const keysByProvider = useMemo(() => {
    const map = new Map<string, ApiKey[]>();
    for (const k of keys) {
      const list = map.get(k.provider_id);
      if (list) list.push(k);
      else map.set(k.provider_id, [k]);
    }
    return map;
  }, [keys]);

  // WebSocket listener for key stats updates (debounced — 后端高频推送时只触发一次重拉)
  const keyStatsTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const handleKeyStats = useCallback(() => {
    if (keyStatsTimer.current) clearTimeout(keyStatsTimer.current);
    keyStatsTimer.current = setTimeout(() => {
      fetchKeys();
    }, 300);
  }, [fetchKeys]);
  useWebSocket('key_stats', handleKeyStats);
  useEffect(() => {
    return () => {
      if (keyStatsTimer.current) clearTimeout(keyStatsTimer.current);
    };
  }, []);

  // Tauri event listeners for plugin status
  useEffect(() => {
    if (!isTauri()) return;

    const unlistenFns: Promise<() => void>[] = [];

    unlistenFns.push(
      tauriListen<{ provider_id: string }>('plugin-online', (payload) => {
        if (payload?.provider_id) {
          setPluginOnlineMap((prev) => ({ ...prev, [payload.provider_id]: true }));
        }
      })
    );

    unlistenFns.push(
      tauriListen<{ provider_id: string }>('plugin-activated', (payload) => {
        if (payload?.provider_id) {
          setPluginOnlineMap((prev) => ({ ...prev, [payload.provider_id]: true }));
        }
      })
    );

    unlistenFns.push(
      tauriListen<{ provider_id: string }>('plugin-offline', (payload) => {
        if (payload?.provider_id) {
          setPluginOnlineMap((prev) => ({ ...prev, [payload.provider_id]: false }));
        }
      })
    );

    return () => {
      unlistenFns.forEach((promise) => {
        promise.then((unlisten) => unlisten());
      });
    };
  }, []);

  // Load initial data
  useEffect(() => {
    const load = async () => {
      setLoading(true);
      try {
        await Promise.all([fetchProviders(), fetchKeys(), fetchModels()]);

        // Load plugin statuses
        if (isTauri()) {
          try {
            const plugins = await pluginsApi.list();
            const map: Record<string, boolean> = {};
            for (const p of plugins) {
              if (p.provider_id) map[p.provider_id] = !!p.connected;
            }
            setPluginOnlineMap(map);
          } catch {
            // ignore
          }
        }
      } finally {
        setLoading(false);
      }
    };

    load();
  }, []);

  // Drag and drop sensors
  const sensors = useSensors(
    useSensor(PointerSensor),
    useSensor(KeyboardSensor, {
      coordinateGetter: sortableKeyboardCoordinates,
    })
  );

  // 排序模式：拖动仅更新本地顺序（乐观更新），点「保存」时才提交后端
  const handleDragEnd = (event: DragEndEvent) => {
    if (!sortMode) return;
    const { active, over } = event;
    if (!over || active.id === over.id) return;

    const oldIndex = providers.findIndex((p) => p.id === active.id);
    const newIndex = providers.findIndex((p) => p.id === over.id);
    useProvidersStore.setState({ providers: arrayMove(providers, oldIndex, newIndex) });
  };

  const handleSaveSort = async () => {
    setSavingSort(true);
    setSortError(null);
    try {
      await reorderProviders(providers.map((p) => p.id));
      setSortMode(false);
    } catch {
      // 保存失败：store 已回滚（重新拉取服务端顺序），保留在排序模式供重试
      setSortError(t('providers.sort_failed'));
    } finally {
      setSavingSort(false);
    }
  };

  const handleEdit = (provider: Provider) => {
    navigate(`/providers/${provider.id}/edit`);
  };

  const handleDelete = (provider: Provider) => {
    setDeleteTarget(provider);
    setDeleteDialogOpen(true);
  };

  const confirmDelete = async () => {
    if (!deleteTarget) return;
    await providersApi.delete(deleteTarget.id);
    setDeleteDialogOpen(false);
    setDeleteTarget(null);
    await fetchProviders();
  };

  return (
    <div className="space-y-6">
      <div className="flex justify-between items-start gap-4 flex-wrap">
        <h2 className="text-3xl font-normal m-0">{t('providers.title')}</h2>
        <div className="flex items-center gap-2.5">
          <Button
            onClick={() => navigate('/providers/new')}
            disabled={sortMode}
          >
            <Plus className="w-4 h-4 mr-2" />
            {t('providers.add')}
          </Button>
          {/* 排序模式切换：排序 → 保存，保存时才把本地顺序提交落库 */}
          <Button
            variant="outline"
            onClick={() => {
              if (sortMode) {
                handleSaveSort();
              } else {
                setSortError(null);
                setSortMode(true);
              }
            }}
            disabled={savingSort}
          >
            {sortMode && <Loader2 className="w-4 h-4 mr-2 animate-spin" />}
            {!sortMode && <ArrowUpDown className="w-4 h-4 mr-2" />}
            {sortMode && !savingSort && <Check className="w-4 h-4 mr-2" />}
            {sortMode ? t('providers.sort_save') : t('providers.sort')}
          </Button>
        </div>
      </div>

      {sortError && <p className="text-sm text-destructive">{sortError}</p>}

      {loading ? (
        <div className="flex flex-col items-center justify-center py-16">
          <div className="animate-spin rounded-full h-8 w-8 border-b-2 border-primary"></div>
        </div>
      ) : providers.length === 0 ? (
        <div className="flex flex-col items-center gap-2 py-16 text-center">
          <Inbox className="w-12 h-12 text-muted-foreground" />
          <p className="text-lg">{t('common.empty')}</p>
        </div>
      ) : (
        <DndContext
          sensors={sensors}
          collisionDetection={closestCenter}
          onDragEnd={handleDragEnd}
        >
          <SortableContext items={providers.map((p) => p.id)} strategy={rectSortingStrategy}>
            <div className="grid grid-cols-[repeat(auto-fill,minmax(320px,1fr))] gap-4">
              {providers.map((provider) => {
                const isPlugin = !!(provider.config as any)?.plugin_id;
                return (
                  <ProviderCard
                    key={provider.id}
                    provider={provider}
                    models={modelsByProvider.get(provider.id) ?? []}
                    keys={keysByProvider.get(provider.id) ?? []}
                    isPlugin={isPlugin}
                    pluginOnline={pluginOnlineMap[provider.id]}
                    sortMode={sortMode}
                    onEdit={() => handleEdit(provider)}
                    onDelete={() => handleDelete(provider)}
                  />
                );
              })}
            </div>
          </SortableContext>
        </DndContext>
      )}

      <Dialog open={deleteDialogOpen} onOpenChange={setDeleteDialogOpen}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>{t('providers.delete_title')}</DialogTitle>
            <DialogDescription>
              {t('providers.delete_confirm', { name: deleteTarget?.name || '' })}
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button variant="outline" onClick={() => setDeleteDialogOpen(false)}>
              {t('common.cancel')}
            </Button>
            <Button variant="destructive" onClick={confirmDelete}>
              {t('providers.delete_confirm_btn')}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}

export default ProvidersView;
