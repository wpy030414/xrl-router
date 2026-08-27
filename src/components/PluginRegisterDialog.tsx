import { useState, useImperativeHandle, forwardRef } from 'react';
import { useNavigate } from 'react-router-dom';
import { useT } from '@/i18n';
import { BASE_URL } from '@/lib/api';
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogDescription,
  DialogFooter,
} from './ui/dialog';
import { Button } from './ui/button';

export interface PluginRegisterDialogHandle {
  show: (data: { plugin_id: string; provider_id: string; provider_name?: string }) => void;
}

export const PluginRegisterDialog = forwardRef<PluginRegisterDialogHandle>((_, ref) => {
  const t = useT();
  const navigate = useNavigate();

  const [visible, setVisible] = useState(false);
  const [providerName, setProviderName] = useState('');
  const [pluginId, setPluginId] = useState('');
  const [providerId, setProviderId] = useState('');

  useImperativeHandle(ref, () => ({
    show: (data: { plugin_id: string; provider_id: string; provider_name?: string }) => {
      setPluginId(data.plugin_id || '');
      setProviderId(data.provider_id || '');
      setProviderName(data.provider_name || data.plugin_id || '');
      setVisible(true);
    },
  }));

  const handleConfirm = () => {
    setVisible(false);
    navigate({
      pathname: '/providers/new',
      search: `?plugin_id=${pluginId}&provider_id=${providerId}`,
    });
  };

  const handleCancel = async () => {
    setVisible(false);
    if (pluginId) {
      try {
        await fetch(`${BASE_URL}/api/plugins/${pluginId}`, { method: 'DELETE' });
      } catch (e) {
        console.error(t('plugin.dialog.ignore'), e);
      }
    }
  };

  return (
    <Dialog open={visible} onOpenChange={setVisible}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{t('plugin.dialog.headline', { name: providerName })}</DialogTitle>
          <DialogDescription>{t('plugin.dialog.desc')}</DialogDescription>
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
});

PluginRegisterDialog.displayName = 'PluginRegisterDialog';
