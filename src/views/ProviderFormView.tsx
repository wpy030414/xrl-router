import { useState, useEffect, useMemo } from 'react';
import { useNavigate, useParams, useSearchParams } from 'react-router';
import { ArrowLeft, Loader2 } from 'lucide-react';
import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';
import { Label } from '@/components/ui/label';
import { Textarea } from '@/components/ui/textarea';
import { Alert } from '@/components/ui/alert';
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

export function ProviderFormView() {
  const t = useT();
  const navigate = useNavigate();
  const { id } = useParams<{ id: string }>();
  const [searchParams] = useSearchParams();
  const { fetchProviders } = useProvidersStore();
  const { keys, fetchKeys, createKey } = useApiKeysStore();

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
  const [modelsText, setModelsText] = useState('');
  const [saving, setSaving] = useState(false);
  // 仅在需要拉取远端数据（编辑 / 插件预填）时进入加载态；新建直接渲染表单。
  const [loading, setLoading] = useState(isEdit || isPlugin);
  const [error, setError] = useState<string | null>(null);
  const [pluginInfo, setPluginInfo] = useState<PluginDetail | null>(null);

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
        if (cfgPluginId) setEditPluginId(cfgPluginId);
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
        setError(t('providerForm.load_failed', { msg: e.message }));
      } finally {
        setLoading(false);
      }
    };

    load();
  }, [id, isEdit, fetchKeys]);

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
        // 插件自带模型列表（注册时已写入 models 表），预填进编辑区
        setModelsText(
          (data.models || [])
            .map((m) => (m.display_name && m.display_name !== m.model_id ? `${m.model_id}<-${m.display_name}` : m.model_id))
            .join('\n')
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

  // Count synced keys
  const syncedKeysCount = useMemo(() => {
    if (!isPlugin) return 0;
    // 新插件模式：注册时同步的密钥数来自插件详情
    if (pluginInfo?.key_count != null) return pluginInfo.key_count;
    // 编辑插件模式：从 keys store 按 provider 过滤（编辑时已 fetchKeys(id)）
    return keys.filter((k) => k.provider_id === id).length;
  }, [pluginInfo, keys, id, isPlugin]);

  const handleSave = async () => {
    if (!name.trim()) return;

    setSaving(true);
    setError(null);

    try {
      const models = parseModelsText(modelsText);
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

        {/* API Key（一行一个）。编辑模式回填明文密钥；插件模式的密钥由插件
            自动同步、不在此编辑，仅显示数量 */}
        {isPlugin ? (
          syncedKeysCount > 0 && (
            <p className="text-sm text-muted-foreground">
              {t('providerForm.keys_synced', { count: syncedKeysCount })}
            </p>
          )
        ) : (
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

        {/* Models */}
        <div className="space-y-1.5">
          <Label htmlFor="provider-models">
            {t('providerForm.models_label')}
          </Label>
          <Textarea
            id="provider-models"
            value={modelsText}
            onChange={(e) => setModelsText(e.target.value)}
            rows={6}
            className="font-mono"
            placeholder={t('providerForm.models_placeholder')}
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
