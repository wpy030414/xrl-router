export type WsEvent =
  | { type: 'key_stats'; provider_id: string; green: number; total: number }
  | { type: 'request_metrics'; provider_id: string; model: string; latency_ms: number; tokens: number }
  | { type: 'balance_update'; provider_id: string; key_id: string; balance: number }
  | { type: 'provider_status'; provider_id: string; status: string; latency_ms: number }
  | { type: 'usage_stats_changed'; timestamp: number }
  | { type: 'error'; provider_id: string; key_id: string; error: string };

type EventHandler = (event: WsEvent) => void;

class WebSocketClient {
  private ws: WebSocket | null = null;
  private handlers: Map<string, Set<EventHandler>> = new Map();
  private reconnectTimer: ReturnType<typeof setTimeout> | null = null;
  private url: string;

  constructor(url: string = 'ws://127.0.0.1:19068/ws') {
    this.url = url;
  }

  connect() {
    if (this.ws?.readyState === WebSocket.OPEN) return;

    this.ws = new WebSocket(this.url);

    this.ws.onmessage = (event) => {
      try {
        const data = JSON.parse(event.data) as WsEvent;
        this.dispatch(data.type, data);
        this.dispatch('*', data);
      } catch {
        // Ignore invalid messages
      }
    };

    this.ws.onclose = () => {
      this.reconnectTimer = setTimeout(() => this.connect(), 3000);
    };

    this.ws.onerror = () => {
      this.ws?.close();
    };
  }

  disconnect() {
    if (this.reconnectTimer) {
      clearTimeout(this.reconnectTimer);
      this.reconnectTimer = null;
    }
    this.ws?.close();
    this.ws = null;
  }

  on(event: string, handler: EventHandler) {
    if (!this.handlers.has(event)) {
      this.handlers.set(event, new Set());
    }
    this.handlers.get(event)!.add(handler);
  }

  off(event: string, handler: EventHandler) {
    this.handlers.get(event)?.delete(handler);
  }

  private dispatch(event: string, data: WsEvent) {
    this.handlers.get(event)?.forEach((h) => h(data));
  }
}

export const wsClient = new WebSocketClient();