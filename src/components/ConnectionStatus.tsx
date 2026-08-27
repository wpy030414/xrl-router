import { useState, useEffect } from 'react';
import { CloudOff, RefreshCw } from 'lucide-react';
import { useT } from '@/i18n';
import { connectionState } from '@/lib/api';
import { Button } from './ui/button';

export function ConnectionStatus() {
  const t = useT();
  const [isOnline, setIsOnline] = useState(connectionState.isOnline);

  useEffect(() => {
    const checkConnection = async () => {
      try {
        const res = await fetch('/health');
        setIsOnline(res.ok);
        connectionState.isOnline = res.ok;
      } catch {
        setIsOnline(false);
        connectionState.isOnline = false;
      }
    };

    checkConnection();
    const interval = setInterval(checkConnection, 5000);
    return () => clearInterval(interval);
  }, []);

  if (isOnline) return null;

  return (
    <div className="fixed top-0 left-0 right-0 z-50 flex items-center justify-center gap-2 px-4 py-2 bg-destructive text-destructive-foreground">
      <CloudOff className="w-4 h-4" />
      <span className="text-sm">{t('conn.offline')}</span>
      <Button
        variant="ghost"
        size="sm"
        onClick={() => window.location.reload()}
        className="h-7 px-2"
      >
        <RefreshCw className="w-3 h-3 mr-1" />
        {t('conn.retry')}
      </Button>
    </div>
  );
}
