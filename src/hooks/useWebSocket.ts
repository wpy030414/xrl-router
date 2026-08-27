import { useEffect } from 'react';
import { wsClient, type WsEvent } from '@/lib/ws';

export function useWebSocket(event: string, handler: (data: WsEvent) => void) {
  useEffect(() => {
    wsClient.connect();
    wsClient.on(event, handler);
    return () => {
      wsClient.off(event, handler);
    };
  }, [event, handler]);
}

export function useKeyHealth(providerId: string, updateKeyHealth: (keyId: string, status: string) => void) {
  useWebSocket('key_health', (data) => {
    if (data.type === 'key_health' && data.provider_id === providerId) {
      updateKeyHealth(data.key_id, data.status);
    }
  });
}
