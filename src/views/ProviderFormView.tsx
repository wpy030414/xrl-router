import { useState, useEffect, useMemo } from 'react';
import { useNavigate, useParams, useSearchParams } from 'react-router-dom';
import { ArrowLeft, Loader2, ChevronDown } from 'lucide-react';
import { Button } from '@/components/ui/button';
import { useProvidersStore } from '@/stores/providers';
import { useKeysStore } from '@/stores/keys';
import { providersApi, modelsApi, keysApi, type Provider } from '@/lib/api';
import { useT } from '@/i18n';
import { cn } from '@/lib/utils';

/** Provider 类型选项 */
const KIND_OPTIONS = [
  { value: 'messages', label: 'Anthropic (Messages)' },
  { value: 'chat_completions', label: 'OpenAI (Chat Completions)' },
  { value: 'responses', label: 'OpenAI (Responses)' },
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

/** 解析模型列表文本为配置 */
function parseModelsText(text: string): { model_id: string; display_name: string }[] {
  return text
    .split('\n')
    .map((line) => line.trim())
    .filter(Boolean)
    .map((line) => {
      const arrowIdx = line.indexOf('<-');
      if (arrowIdx !== -1) {
        const model_id = line.slice(0, arrowIdx).trim();
        const display_name = line.slice(arrowIdx + 2).trim();
        return { model_id, display_name };
      }
      return { model_id: line, display_name: line };
    });
}

/** 将模型配置转为文本 */
function modelsToText(models: { model_id: string; display_name: string }[]): string {
  return models
    .map((m) => (m.model_id === m.display_name ? m.model_id : `${m.model_id}<-${m.display_name}`))
    .join('\n');
}

/** 解析密钥文本为明文密钥数组（一行一个，忽略空行）。 */
function parseKeysText(text: string): string[] {
  return text
    .split('\n')
    .map((line) => line.trim())
    .filter(Boolean);
}

interface PluginInfo {
  plugin_id: string;
  provider_id: string;
  name?: string;
  kind?: string;
  base_url?: string;
}

export function ProviderFormView() {
  const t = useT();
  const navigate = useNavigate();
  const { id } = useParams<{ id: string }>();
  const [searchParams] = useSearchParams();
  const { fetchProviders } = useProvidersStore();
  const { keys, fetchKeys, createKey } = useKeysStore();

  const isEdit = !!id;
  const pluginId = searchParams.get('plugin_id');
  const pluginProviderId = searchParams.get('provider_id');
  const isPlugin = !!pluginId;

  // Form state
  const [name, setName] = useState('');
  const [kind, setKind] = useState<Provider['kind']>('messages');
  const [baseUrl, setBaseUrl] = useState(DEFAULT_URLS.messages);
  const [apiPath, setApiPath] = useState(DEFAULT_PATHS.messages);
  const [apiKeysText, setApiKeysText] = useState('');
  const [modelsText, setModelsText] = useState('');
  const [saving, setSaving] = useState(false);
  // 仅在需要拉取远端数据（编辑 / 插件预填）时进入加载态；新建直接渲染表单。
  const [loading, setLoading] = useState(isEdit || isPlugin);
  const [error, setError] = useState<string | null>(null);
  const [pluginInfo, setPluginInfo] = useState<PluginInfo | null>(null);

  // Load existing provider data for edit mode
  useEffect(() => {
    if (!isEdit) return;

    const load = async () => {
      setLoading(true);
      try {
        const provider = await providersApi.get(id);
        setName(provider.name);
        setKind(provider.kind);
        setBaseUrl(provider.base_url);
        setApiPath(provider.api_path);
        // 模型以 models 表为准（代理按该表路由）；config.models 是旧版
        // 新 UI 的写入位置，仅在表里没有数据时作兼容回退。
        let models = await modelsApi.list(id);
        if (models.length === 0) {
          models = (provider.config?.models || []) as { model_id: string; display_name: string }[];
        }
        setModelsText(modelsToText(models));
        // 回填明文密钥（一行一个）；插件模式密钥由插件托管，仅显示数量
        if (!provider.config?.plugin_id) {
          const keys = await keysApi.list(id);
          setApiKeysText(keys.map((k) => k.key_plain || '').filter(Boolean).join('\n'));
        }
        // 拉取密钥列表，供插件模式显示「已自动同步」数量
        await fetchKeys(id);
      } catch (e: any) {
        setError(t('providerNew.load_failed', { msg: e.message }));
      } finally {
        setLoading(false);
      }
    };

    load();
  }, [id, isEdit, fetchKeys]);

  // Load plugin info for plugin mode
  useEffect(() => {
    if (!isPlugin) return;

    const load = async () => {
      setLoading(true);
      try {
        const resp = await fetch(`/api/plugins`);
        if (resp.ok) {
          const plugins = await resp.json();
          const plugin = plugins.find((p: any) => p.plugin_id === pluginId);
          if (plugin) {
            setPluginInfo({
              plugin_id: plugin.plugin_id,
              provider_id: pluginProviderId || plugin.provider_id || '',
              name: plugin.provider_name || plugin.plugin_id,
              kind: plugin.kind || 'chat_completions',
              base_url: plugin.base_url || '',
            });
            setName(plugin.provider_name || plugin.plugin_id);
            setKind(plugin.kind || 'chat_completions');
            setBaseUrl(plugin.base_url || '');
            setApiPath(plugin.api_path || DEFAULT_PATHS[plugin.kind || 'chat_completions']);
            // 插件自带模型列表（注册时已写入 models 表），预填进编辑区
            setModelsText(
              ((plugin.models || []) as { model_id: string; display_name: string }[])
                .map((m) => (m.display_name && m.display_name !== m.model_id ? `${m.model_id}<-${m.display_name}` : m.model_id))
                .join('\n')
            );
          }
        }
      } catch (e: any) {
        setError(t('providerNew.plugin_load_failed', { msg: e.message }));
      } finally {
        setLoading(false);
      }
    };

    load();
  }, [pluginId, isPlugin]);

  // Update default URL and path when kind changes (only in create mode)
  const handleKindChange = (newKind: Provider['kind']) => {
    setKind(newKind);
    if (!isEdit && !isPlugin) {
      setBaseUrl(DEFAULT_URLS[newKind]);
      setApiPath(DEFAULT_PATHS[newKind]);
    }
  };

  // Count synced keys
  const syncedKeysCount = useMemo(() => {
    if (!isEdit) return 0;
    return keys.filter((k) => k.provider_id === id).length;
  }, [keys, id, isEdit]);

  const handleSave = async () => {
    if (!name.trim()) return;

    setSaving(true);
    setError(null);

    try {
      const models = parseModelsText(modelsText);
      const config: Record<string, any> = { models };

      if (isPlugin && pluginInfo) {
        config.plugin_id = pluginInfo.plugin_id;
      }

      const data: Partial<Provider> = {
        name: name.trim(),
        kind,
        base_url: baseUrl.trim(),
        api_path: apiPath.trim(),
        enabled: true,
        config,
      };

      let savedProvider: Provider;

      if (isEdit) {
        savedProvider = await providersApi.update(id, data);
      } else {
        savedProvider = await providersApi.create(data);
      }

      // 模型全量对账到 models 表（代理按该表路由，config.models 只是记录）：
      // 新增缺失的、同步别名改名的，仅删除从输入里移除的——已在用的模型
      // 不做删旧建新，避免 usage_log 的 model_id 外键引用被清掉。
      const existingModels = await modelsApi.list(savedProvider.id);
      const wantModels = new Map(models.map((m) => [m.model_id, m]));
      for (const m of existingModels) {
        if (!wantModels.has(m.model_id)) {
          await modelsApi.delete(m.id);
        }
      }
      for (const [modelId, m] of wantModels) {
        const displayName = m.display_name || modelId;
        const ex = existingModels.find((e) => e.model_id === modelId);
        if (!ex) {
          await modelsApi.create({
            provider_id: savedProvider.id,
            model_id: modelId,
            display_name: displayName,
            tier: 'custom',
          });
        } else if (ex.display_name !== displayName) {
          await modelsApi.update(ex.id, { display_name: displayName });
        }
      }

      // API Key（一行一个）全量对账：新增缺失的、删除从输入里移除的；
      // 明文相同的保留原 key id，不破坏用量统计归属。插件模式跳过——
      // 密钥由插件从 .env 自动同步到密钥池。
      if (!isPlugin) {
        try {
          const inputKeys = parseKeysText(apiKeysText);
          const existingKeys = await keysApi.list(savedProvider.id);
          const existingPlain = new Set(existingKeys.map((k) => k.key_plain || '').filter(Boolean));
          for (const line of inputKeys) {
            if (!existingPlain.has(line)) {
              await createKey(savedProvider.id, {
                name: name.trim() + t('providerNew.key_suffix'),
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

      await fetchProviders();
      navigate('/providers');
    } catch (e: any) {
      setError(t('providerNew.save_failed', { msg: e.message }));
    } finally {
      setSaving(false);
    }
  };

  const title = isPlugin
    ? t('providerNew.title.plugin')
    : isEdit
    ? t('providerNew.title.edit')
    : t('providerNew.title.create');

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
        <div className="rounded-lg bg-destructive/10 text-destructive px-4 py-3 text-sm">
          {error}
        </div>
      )}

      {/* Form */}
      <div className="space-y-5">
        {/* Name */}
        <div className="space-y-1.5">
          <label className="text-sm font-medium" htmlFor="provider-name">
            {t('providerNew.name_label')}
          </label>
          <input
            id="provider-name"
            type="text"
            value={name}
            onChange={(e) => setName(e.target.value)}
            disabled={isPlugin}
            className={cn(
              'flex h-10 w-full rounded-md border border-input bg-background px-3 py-2 text-sm',
              'placeholder:text-muted-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring',
              'disabled:cursor-not-allowed disabled:opacity-50'
            )}
            placeholder="My Provider"
          />
        </div>

        {/* Kind */}
        <div className="space-y-1.5">
          <label className="text-sm font-medium" htmlFor="provider-kind">
            {t('providerNew.kind_label')}
          </label>
          <div className="relative">
            <select
              id="provider-kind"
              value={kind}
              onChange={(e) => handleKindChange(e.target.value as Provider['kind'])}
              disabled={isEdit || isPlugin}
              className={cn(
                // appearance-none：去掉原生 select 自带内边距，px-3 才能与 input 精确对齐；
                // pr-9 给右侧 chevron 留位
                'flex h-10 w-full appearance-none rounded-md border border-input bg-background px-3 py-2 pr-9 text-sm',
                'focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring',
                'disabled:cursor-not-allowed disabled:opacity-50'
              )}
            >
              {KIND_OPTIONS.map((opt) => (
                <option key={opt.value} value={opt.value}>
                  {opt.label}
                </option>
              ))}
            </select>
            <ChevronDown className="pointer-events-none absolute right-3 top-1/2 h-4 w-4 -translate-y-1/2 text-muted-foreground" />
          </div>
        </div>

        {/* Base URL */}
        <div className="space-y-1.5">
          <label className="text-sm font-medium" htmlFor="provider-base-url">
            {t('providerNew.base_url_label')}
          </label>
          <input
            id="provider-base-url"
            type="url"
            value={baseUrl}
            onChange={(e) => setBaseUrl(e.target.value)}
            className={cn(
              'flex h-10 w-full rounded-md border border-input bg-background px-3 py-2 text-sm font-mono',
              'placeholder:text-muted-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring',
              'disabled:cursor-not-allowed disabled:opacity-50'
            )}
            placeholder="https://api.example.com"
          />
        </div>

        {/* API Key（一行一个）。编辑模式回填明文密钥；插件模式的密钥由插件
            自动同步、不在此编辑，仅显示数量 */}
        {isPlugin ? (
          syncedKeysCount > 0 && (
            <p className="text-sm text-muted-foreground">
              {t('providerNew.keys_synced', { count: syncedKeysCount })}
            </p>
          )
        ) : (
          <div className="space-y-1.5">
            <label className="text-sm font-medium" htmlFor="provider-api-key">
              {t('providerNew.api_key_label')}
            </label>
            <textarea
              id="provider-api-key"
              value={apiKeysText}
              onChange={(e) => setApiKeysText(e.target.value)}
              rows={3}
              autoComplete="off"
              spellCheck={false}
              className={cn(
                'flex w-full rounded-md border border-input bg-background px-3 py-2 text-sm font-mono',
                'placeholder:text-muted-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring',
                'resize-y min-h-[72px]'
              )}
              placeholder="sk-ant-..."
            />
          </div>
        )}

        {/* Models */}
        <div className="space-y-1.5">
          <label className="text-sm font-medium" htmlFor="provider-models">
            {t('providerNew.models_label')}
          </label>
          <textarea
            id="provider-models"
            value={modelsText}
            onChange={(e) => setModelsText(e.target.value)}
            rows={6}
            className={cn(
              'flex w-full rounded-md border border-input bg-background px-3 py-2 text-sm font-mono',
              'placeholder:text-muted-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring',
              'resize-y min-h-[120px]'
            )}
            placeholder={t('providerNew.models_placeholder')}
          />
        </div>

        {/* Actions */}
        <div className="flex items-center gap-3 pt-2">
          <Button variant="outline" onClick={() => navigate('/providers')}>
            {t('common.cancel')}
          </Button>
          <Button onClick={handleSave} disabled={saving || !name.trim()}>
            {saving && <Loader2 className="w-4 h-4 mr-2 animate-spin" />}
            {saving
              ? t('providerNew.saving')
              : isEdit
              ? t('providerNew.save_edit')
              : t('providerNew.save_create')}
          </Button>
        </div>
      </div>
    </div>
  );
}
