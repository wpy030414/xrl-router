import { useEffect, useState } from 'react';
import { useNavigate } from 'react-router';
import { useT } from '@/i18n';
import { pluginsApi } from '@/lib/api';
import { listen } from '@/lib/tauri';
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogDescription,
  DialogFooter,
} from './ui/dialog';
import { Button } from './ui/button';

/** 与 registry.rs emit 的 plugin-register payload 对齐 */
interface PluginRegisterPayload {
  plugin_id: string;
  provider_id: string;
  provider_name?: string;
  kind?: string;
  base_url?: string;
  api_path?: string;
  models?: { model_id: string; display_name: string; tier: string }[];
  key_count?: number;
}

/** 与 ProviderFormView 的 KIND_OPTIONS 标签保持一致 */
const KIND_LABELS: Record<string, string> = {
  messages: 'Anthropic (Messages)',
  chat_completions: 'OpenAI (Chat Completions)',
  responses: 'OpenAI (Responses)',
};

export function PluginRegisterDialog() {
  const t = useT();
  const navigate = useNavigate();

  const [visible, setVisible] = useState(false);
  const [info, setInfo] = useState<PluginRegisterPayload | null>(null);

  // 插件注册事件仅桌面端可收（listen 内部已守卫 isTauri()）。
  // 重复注册时 last-wins（与 Vue 版 show() 覆写行为一致）。
  useEffect(() => {
    const unlistenPromise = listen<PluginRegisterPayload>('plugin-register', (payload) => {
      console.log('[Plugin] Register event:', payload);
      setInfo(payload);
      setVisible(true);
    });
    return () => {
      unlistenPromise.then((unlisten) => unlisten());
    };
  }, []);

  const handleConfirm = () => {
    setVisible(false);
    if (!info?.plugin_id) return;
    navigate({
      pathname: '/providers/new',
      search: `?plugin_id=${info.plugin_id}&provider_id=${info.provider_id}`,
    });
  };

  const handleCancel = async () => {
    setVisible(false);
    // 忽略 = 彻底删除：删除插件记录 + 关联 provider + 模型。
    // 下次插件重连时会重新注册、重新弹窗。
    if (info?.plugin_id) {
      try {
        await pluginsApi.remove(info.plugin_id);
      } catch (e) {
        console.error(t('plugin.dialog.ignore'), e);
      }
    }
  };

  const detailParts: string[] = [];
  if (info?.kind) detailParts.push(t('plugin.dialog.kind', { kind: KIND_LABELS[info.kind] ?? info.kind }));
  if (info?.base_url) detailParts.push(t('plugin.dialog.base_url', { url: info.base_url }));
  if (info?.models && info.models.length > 0) detailParts.push(t('plugin.dialog.models', { count: info.models.length }));
  if (typeof info?.key_count === 'number' && info.key_count > 0) {
    detailParts.push(t('plugin.dialog.keys', { count: info.key_count }));
  }

  return (
    <Dialog open={visible} onOpenChange={setVisible}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{t('plugin.dialog.headline', { name: info?.provider_name || info?.plugin_id || '' })}</DialogTitle>
          <DialogDescription>{t('plugin.dialog.desc')}</DialogDescription>
          {detailParts.length > 0 && (
            <p className="text-sm text-muted-foreground">{detailParts.join(' · ')}</p>
          )}
        </DialogHeader>
        <DialogFooter>
          <Button variant="ghost" onClick={handleCancel}>
            {t('plugin.dialog.ignore')}
          </Button>
          <Button onClick={handleConfirm}>{t('plugin.dialog.add')}</Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
