import { useEffect, useRef } from 'react';
import { wsClient, type WsEvent } from '@/lib/ws';

export function useWebSocket(event: string, handler: (data: WsEvent) => void) {
  // handler 存进 ref：调用方每次 render 新建箭头函数时不必重挂订阅，
  // 也就不会造成「切换视图 → 订阅抖动」的额外开销。
  const ref = useRef(handler);
  ref.current = handler;

  useEffect(() => {
    wsClient.connect();
    const wrapped: (data: WsEvent) => void = (data) => ref.current(data);
    wsClient.on(event, wrapped);
    return () => {
      wsClient.off(event, wrapped);
    };
  }, [event]);
}
