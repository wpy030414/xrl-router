export type WsEvent =
  | { type: 'key_stats'; provider_id: string; green: number; total: number }
  | { type: 'usage_stats_changed'; timestamp: number }
  | { type: 'local_status'; id: string; model_id: string; status: string; port: number | null; error: string | null };

type EventHandler = (event: WsEvent) => void;

class WebSocketClient {
  private ws: WebSocket | null = null;
  private handlers: Map<string, Set<EventHandler>> = new Map();
  private reconnectTimer: ReturnType<typeof setTimeout> | null = null;
  /**
   * 调用方是否要求保持下线。true = 没有连接、也不打算自动重连。
   * 只有区分「主动断开」与「链路故障」，onclose 才不会在调用方明确要停的时候
   * 继续 3s 后偷偷重连。
   */
  private wantsDown = true;
  private url: string;

  constructor(url: string = 'ws://127.0.0.1:19068/ws') {
    this.url = url;
  }

  connect() {
    this.wantsDown = false;
    // CONNECTING 中的 socket 既不能重建、也不能 close()：
    // 后者会被浏览器报 "WebSocket is closed before the connection is established"。
    if (
      this.ws &&
      (this.ws.readyState === WebSocket.CONNECTING || this.ws.readyState === WebSocket.OPEN)
    ) {
      return;
    }

    const ws = new WebSocket(this.url);
    this.ws = ws;

    ws.onmessage = (event) => {
      try {
        const data = JSON.parse(event.data) as WsEvent;
        this.dispatch(data.type, data);
        this.dispatch('*', data);
      } catch {
        // Ignore invalid messages
      }
    };

    ws.onclose = () => {
      if (this.ws === ws) this.ws = null;
      if (this.wantsDown) return;
      this.scheduleReconnect();
    };

    // 不在 onerror 里 close()：浏览器随后会自己触发 onclose，由那条路径统一重连。
    // 提前 close 一个 CONNECTING 中的连接只会多刷一条无意义的控制台报错。
    ws.onerror = () => {
      /* 交给 onclose */
    };
  }

  disconnect() {
    this.wantsDown = true;
    if (this.reconnectTimer) {
      clearTimeout(this.reconnectTimer);
      this.reconnectTimer = null;
    }
    const ws = this.ws;
    this.ws = null;
    if (!ws) return;
    // 先摘掉回调再 close，避免自己触发的 onclose 又排一次重连。
    ws.onclose = null;
    ws.onerror = null;
    ws.onmessage = null;
    ws.close();
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

  private scheduleReconnect() {
    if (this.reconnectTimer) return;
    this.reconnectTimer = setTimeout(() => {
      this.reconnectTimer = null;
      this.connect();
    }, 3000);
  }

  private dispatch(event: string, data: WsEvent) {
    this.handlers.get(event)?.forEach((h) => h(data));
  }
}

export const wsClient = new WebSocketClient();
