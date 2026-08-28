import { useState, useEffect } from 'react';
import { useNavigate, useParams } from 'react-router-dom';
import { ArrowLeft, Loader2, ChevronUp, ChevronDown, X, Plus } from 'lucide-react';
import { Button } from '@/components/ui/button';
import { useCombosStore } from '@/stores/combos';
import { useModelsStore } from '@/stores/models';
import { useProvidersStore } from '@/stores/providers';
import { combosApi, type Combo } from '@/lib/api';
import { useT } from '@/i18n';
import { cn } from '@/lib/utils';

interface ModelOption {
  id: string;
  display_name: string;
  provider_name: string;
}

export function ComboFormView() {
  const t = useT();
  const navigate = useNavigate();
  const { id } = useParams<{ id: string }>();
  const isEdit = !!id;

  const { fetchCombos } = useCombosStore();
  const { models, fetchModels } = useModelsStore();
  const { providers, fetchProviders } = useProvidersStore();

  // 仅在编辑（需拉取远端数据）时进入加载态；新建直接渲染表单。
  const [loading, setLoading] = useState(isEdit);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Form fields
  const [name, setName] = useState('');
  const [enabled, setEnabled] = useState(true);
  const [selectedMembers, setSelectedMembers] = useState<string[]>([]);
  const [availableModels, setAvailableModels] = useState<ModelOption[]>([]);

  // Load existing combo for edit mode
  useEffect(() => {
    if (!isEdit || !id) return;

    const load = async () => {
      setLoading(true);
      try {
        const combo = await combosApi.get(id);
        setName(combo.name);
        setEnabled(combo.enabled);
        setSelectedMembers(combo.members);
      } catch (e: any) {
        setError(t('comboNew.load_failed', { msg: e.message }));
      } finally {
        setLoading(false);
      }
    };

    load();
  }, [id, isEdit]);

  // Load available models from all providers
  useEffect(() => {
    const loadModels = async () => {
      // 并行拉取 models 和 providers
      await Promise.all([fetchModels(), fetchProviders()]);
      // 从 store 最新状态读取，闭包里的值仍是旧值（首次渲染为空数组）
      const currentModels = useModelsStore.getState().models;
      const currentProviders = useProvidersStore.getState().providers;

      // 构建 provider id -> name 映射
      const providerMap = new Map(currentProviders.map((p) => [p.id, p.name]));

      // 组合成 ModelOption，用 display_name 作为成员标识（后端校验用 display_name）
      const options: ModelOption[] = currentModels.map((m) => ({
        id: m.display_name, // combo members 存的是 display_name
        display_name: m.display_name,
        provider_name: providerMap.get(m.provider_id) || 'Unknown',
      }));
      setAvailableModels(options);
    };

    loadModels();
  }, [fetchModels, fetchProviders]);

  // Add model to selected members
  const handleAddModel = (modelId: string) => {
    if (!selectedMembers.includes(modelId)) {
      setSelectedMembers([...selectedMembers, modelId]);
    }
  };

  // Remove model from selected members
  const handleRemoveModel = (modelId: string) => {
    setSelectedMembers(selectedMembers.filter((m) => m !== modelId));
  };

  // Move model up
  const handleMoveUp = (index: number) => {
    if (index === 0) return;
    const newMembers = [...selectedMembers];
    [newMembers[index - 1], newMembers[index]] = [newMembers[index], newMembers[index - 1]];
    setSelectedMembers(newMembers);
  };

  // Move model down
  const handleMoveDown = (index: number) => {
    if (index === selectedMembers.length - 1) return;
    const newMembers = [...selectedMembers];
    [newMembers[index], newMembers[index + 1]] = [newMembers[index + 1], newMembers[index]];
    setSelectedMembers(newMembers);
  };

  // Save combo
  const handleSave = async () => {
    if (!name.trim()) return;
    if (selectedMembers.length === 0) return;

    setSaving(true);
    setError(null);

    try {
      if (isEdit && id) {
        await combosApi.update(id, {
          name: name.trim(),
          enabled,
          members: selectedMembers,
        });
      } else {
        await combosApi.create({
          name: name.trim(),
          enabled,
          members: selectedMembers,
        });
      }

      await fetchCombos();
      navigate('/combos');
    } catch (e: any) {
      setError(t('comboNew.save_failed', { msg: e.message }));
    } finally {
      setSaving(false);
    }
  };

  if (loading) {
    return (
      <div className="flex items-center justify-center py-16">
        <Loader2 className="w-6 h-6 animate-spin text-muted-foreground" />
      </div>
    );
  }

  const title = isEdit ? t('comboNew.title.edit') : t('comboNew.title.create');

  return (
    <div className="space-y-6">
      {/* Header */}
      <div className="flex items-center gap-3">
        <Button variant="ghost" size="icon" onClick={() => navigate('/combos')}>
          <ArrowLeft className="w-5 h-5" />
        </Button>
        <h2 className="text-3xl font-normal m-0">{title}</h2>
      </div>

      {/* Error banner */}
      {error && (
        <div className="rounded-lg bg-destructive/10 text-destructive px-4 py-3 text-sm">
          {error}
        </div>
      )}

      {/* Form */}
      <div className="space-y-5">
        {/* Name */}
        <div className="space-y-1.5">
          <label className="text-sm font-medium" htmlFor="combo-name">
            {t('comboNew.name_label')}
          </label>
          <input
            id="combo-name"
            type="text"
            value={name}
            onChange={(e) => setName(e.target.value)}
            className={cn(
              'flex h-10 w-full rounded-md border border-input bg-background px-3 py-2 text-sm',
              'placeholder:text-muted-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring'
            )}
            placeholder="My Combo"
          />
        </div>

        {/* Enabled toggle */}
        <div className="flex items-center gap-3">
          <label className="text-sm font-medium">{t('comboNew.enabled_label')}</label>
          <button
            type="button"
            onClick={() => setEnabled(!enabled)}
            className={cn(
              'relative inline-flex h-6 w-11 items-center rounded-full transition-colors',
              enabled ? 'bg-primary' : 'bg-muted-foreground/20'
            )}
          >
            <span
              className={cn(
                'inline-block h-4 w-4 transform rounded-full bg-white transition-transform',
                enabled ? 'translate-x-6' : 'translate-x-1'
              )}
            />
          </button>
          <span className="text-sm text-muted-foreground">
            {enabled ? t('common.enabled') : t('common.disabled')}
          </span>
        </div>

        {/* Selected members */}
        <div className="space-y-2">
          <label className="text-sm font-medium">{t('comboNew.selected_label')}</label>
          {selectedMembers.length === 0 ? (
            <p className="text-sm text-muted-foreground italic py-4 text-center border rounded-lg">
              {t('comboNew.selected_empty')}
            </p>
          ) : (
            <div className="space-y-2">
              {selectedMembers.map((memberId, index) => {
                const model = availableModels.find((m) => m.id === memberId);
                return (
                  <div
                    key={memberId}
                    className="flex items-center gap-2 p-3 bg-muted rounded-lg"
                  >
                    <span className="flex-shrink-0 w-6 h-6 rounded-full bg-primary/10 text-primary flex items-center justify-center text-xs font-bold">
                      {index + 1}
                    </span>
                    <div className="flex-1 min-w-0">
                      <p className="font-medium truncate">
                        {model?.display_name || memberId}
                      </p>
                      <p className="text-xs text-muted-foreground truncate">
                        {model?.provider_name || t('common.unknown')}
                      </p>
                    </div>
                    <div className="flex gap-1">
                      <Button
                        variant="ghost"
                        size="icon"
                        className="w-8 h-8"
                        onClick={() => handleMoveUp(index)}
                        disabled={index === 0}
                      >
                        <ChevronUp className="w-4 h-4" />
                      </Button>
                      <Button
                        variant="ghost"
                        size="icon"
                        className="w-8 h-8"
                        onClick={() => handleMoveDown(index)}
                        disabled={index === selectedMembers.length - 1}
                      >
                        <ChevronDown className="w-4 h-4" />
                      </Button>
                      <Button
                        variant="ghost"
                        size="icon"
                        className="w-8 h-8 text-destructive"
                        onClick={() => handleRemoveModel(memberId)}
                      >
                        <X className="w-4 h-4" />
                      </Button>
                    </div>
                  </div>
                );
              })}
            </div>
          )}
        </div>

        {/* Available models */}
        <div className="space-y-2">
          <label className="text-sm font-medium">{t('comboNew.available_label')}</label>
          {availableModels.length === 0 ? (
            <p className="text-sm text-muted-foreground italic py-4 text-center border rounded-lg">
              {t('comboNew.no_models')}
            </p>
          ) : (
            <div className="grid grid-cols-1 md:grid-cols-2 gap-2 max-h-[300px] overflow-y-auto border rounded-lg p-3">
              {availableModels.map((model) => {
                const isSelected = selectedMembers.includes(model.id);
                return (
                  <button
                    key={model.id}
                    type="button"
                    onClick={() => handleAddModel(model.id)}
                    disabled={isSelected}
                    className={cn(
                      'flex items-center gap-2 p-2 rounded-md text-left transition-colors',
                      isSelected
                        ? 'bg-muted cursor-not-allowed opacity-50'
                        : 'hover:bg-muted cursor-pointer'
                    )}
                  >
                    <Plus className="w-4 h-4 flex-shrink-0" />
                    <div className="flex-1 min-w-0">
                      <p className="font-medium text-sm truncate">{model.display_name}</p>
                      <p className="text-xs text-muted-foreground truncate">
                        {model.provider_name}
                      </p>
                    </div>
                  </button>
                );
              })}
            </div>
          )}
        </div>

        {/* Actions */}
        <div className="flex items-center gap-3 pt-2">
          <Button variant="outline" onClick={() => navigate('/combos')}>
            {t('common.cancel')}
          </Button>
          <Button
            onClick={handleSave}
            disabled={saving || !name.trim() || selectedMembers.length === 0}
          >
            {saving && <Loader2 className="w-4 h-4 mr-2 animate-spin" />}
            {saving
              ? t('comboNew.saving')
              : isEdit
              ? t('comboNew.save_edit')
              : t('comboNew.save_create')}
          </Button>
        </div>
      </div>
    </div>
  );
}

export default ComboFormView;
