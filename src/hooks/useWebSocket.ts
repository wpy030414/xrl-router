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
