// Dynamic BASE_URL: use current origin for LAN browsers, 127.0.0.1 for Tauri/local dev.
// 注意用 127.0.0.1 而非 localhost：Windows 上 localhost 优先解析为 ::1（后端只绑 IPv4），
// 且代理工具（Clash 等）的 bypass 规则通常覆盖 127.0.0.1 而未必覆盖 localhost——
// 后者被代理劫持时响应是 HTML，会导致 JSON.parse 报 "Unexpected token '<'"。
function getBaseUrl(): string {
  if (typeof window === 'undefined') return 'http://127.0.0.1:19068';

  const { hostname, protocol, port } = window.location;

  // Tauri WebView or local development
  // 注意：Tauri 2 在 Windows 生产模式用 http://tauri.localhost（hostname=tauri.localhost，
  // protocol=http:），macOS/Linux 用 tauri://localhost（protocol=tauri:）——两者都必须
  // 命中本分支。漏掉 tauri.localhost 会落到下方 LAN 分支拼出 http://tauri.localhost，
  // 请求打到 Tauri asset protocol 返回 index.html（200 HTML），JSON.parse 报
  // "Unexpected token '<'"。dev 模式页面是 localhost:5173，故只有构建版触发。
  if (
    hostname === 'localhost' ||
    hostname === '127.0.0.1' ||
    hostname.endsWith('.localhost') ||
    protocol === 'tauri:'
  ) {
    return 'http://127.0.0.1:19068';
  }

  // LAN browser: use same origin to avoid CORS
  return `${protocol}//${hostname}:${port}`;
}

export const BASE_URL = getBaseUrl();

interface RequestOptions {
  method?: string;
  body?: unknown;
}

// Connection state for offline detection
export const connectionState = {
  isOnline: true,
  lastCheck: 0,
};

async function request<T>(path: string, opts: RequestOptions = {}): Promise<T> {
  const headers: Record<string, string> = {
    'Content-Type': 'application/json',
  };

  try {
    const res = await fetch(`${BASE_URL}${path}`, {
      method: opts.method || 'GET',
      headers,
      body: opts.body ? JSON.stringify(opts.body) : undefined,
    });

    if (!res.ok) {
      let errorDetail = `${res.status} ${res.statusText}`;
      try {
        const errBody = await res.json();
        if (errBody?.error?.message) {
          errorDetail = errBody.error.message;
        } else if (errBody?.error) {
          errorDetail = typeof errBody.error === 'string' ? errBody.error : JSON.stringify(errBody.error);
        }
      } catch {
        // ignore parse errors
      }
      throw new Error(`API error: ${errorDetail}`);
    }

    connectionState.isOnline = true;
    connectionState.lastCheck = Date.now();
    const text = await res.text();
    try {
      return text ? JSON.parse(text) : ({} as T);
    } catch {
      // 2xx 但不是 JSON：多半是请求被代理/端口占用者劫持，返回了 HTML 页。
      // 把响应特征带进错误，便于直接定位来源（vite index.html / 代理拦截页等）。
      const snippet = text.slice(0, 200).replace(/\s+/g, ' ').trim();
      const ctype = res.headers.get('content-type') || 'unknown';
      throw new Error(
        `API error: 响应不是 JSON（content-type: ${ctype}，请求 ${opts.method || 'GET'} ${path}）: ${snippet}`,
      );
    }
  } catch (err: any) {
    // Network error = offline
    if (err.name === 'TypeError' || err.message?.includes('fetch')) {
      connectionState.isOnline = false;
    }
    throw err;
  }
}

// --- Providers ---
export interface Provider {
  id: string;
  name: string;
  kind: 'messages' | 'chat_completions' | 'responses';
  base_url: string;
  api_path: string;
  enabled: boolean;
  config: Record<string, any>;
  created_at: number;
  updated_at: number;
}

export const providersApi = {
  list: () => request<Provider[]>('/api/providers'),
  get: (id: string) => request<Provider>(`/api/providers/${id}`),
  create: (data: Partial<Provider>) => request<Provider>('/api/providers', { method: 'POST', body: data }),
  update: (id: string, data: Partial<Provider>) => request<Provider>(`/api/providers/${id}`, { method: 'PUT', body: data }),
  delete: (id: string) => request<{ status: string }>(`/api/providers/${id}`, { method: 'DELETE' }),
  reorder: (ids: string[]) => request<{ status: string }>('/api/providers/reorder', { method: 'PUT', body: { ids } }),
};

// --- Service Keys ---
export interface ServiceKey {
  id: string;
  name: string;
  key_masked: string;
  allowed_models: string[];
  /** 5h 滚动窗口 token 上限，0 = 不设限 */
  quota_5h: number;
  /** 7d 滚动窗口 token 上限，0 = 不设限 */
  quota_7d: number;
  total_requests: number;
  total_tokens: number;
  last_used_at: number | null;
  created_at: number;
  updated_at: number;
}

/** list 响应附带的滚动窗口已用量 */
export interface ServiceKeyUsage {
  /** 5h 滚动窗口已用 tokens */
  used_5h: number;
  /** 7d 滚动窗口已用 tokens */
  used_7d: number;
}

export const serviceKeysApi = {
  list: () => request<(ServiceKey & ServiceKeyUsage)[]>('/api/service-keys'),
  create: (data: { name: string }) =>
    request<{ ok: boolean; id: string; key: string }>('/api/service-keys', { method: 'POST', body: data }),
  update: (id: string, data: { name?: string; allowed_models?: string[]; quota_5h?: number; quota_7d?: number }) =>
    request<{ ok: boolean }>(`/api/service-keys/${id}`, { method: 'PUT', body: data }),
  delete: (id: string) => request<{ ok: boolean }>(`/api/service-keys/${id}`, { method: 'DELETE' }),
};

// --- API Keys (provider keys) ---
export interface ApiKey {
  id: string;
  provider_id: string;
  name: string;
  key_masked: string;
  key_plain?: string;
  status: 'green' | 'yellow' | 'red' | 'unknown';
  last_error: string | null;
  last_error_code: number | null;
  last_used_at: number | null;
  created_at: number;
  balance: number | null;
  total_requests: number;
  total_tokens: number;
}

export const keysApi = {
  list: (providerId?: string) => {
    const base = '/api/keys';
    return request<ApiKey[]>(providerId ? `${base}?provider_id=${providerId}` : base);
  },
  create: (providerId: string, data: { name: string; key: string }) =>
    request<ApiKey>(`/api/keys`, { method: 'POST', body: { ...data, provider_id: providerId } }),
  update: (id: string, data: Partial<ApiKey>) =>
    request<ApiKey>(`/api/keys/${id}`, { method: 'PUT', body: data }),
  delete: (providerId: string, keyId: string) =>
    request<{ ok: boolean }>(`/api/keys/${keyId}`, { method: 'DELETE' }),
};

// --- Models ---
export interface Model {
  id: string;
  provider_id: string;
  model_id: string;
  display_name: string;
  tier: 'fable' | 'opus' | 'sonnet' | 'haiku' | 'custom';
  context_window: number;
  max_output_tokens: number;
  enabled: boolean;
}

export const modelsApi = {
  list: (providerId?: string) => {
    const base = '/api/models';
    return request<Model[]>(providerId ? `${base}?provider_id=${providerId}` : base);
  },
  sync: (providerId: string) =>
    request<{ ok: boolean }>(`/api/models/sync`, { method: 'POST', body: { provider_id: providerId } }),
  create: (data: any) =>
    request<Model>(`/api/models`, { method: 'POST', body: data }),
  update: (modelId: string, data: any) =>
    request<Model>(`/api/models/${modelId}`, { method: 'PUT', body: data }),
  delete: (modelId: string) =>
    request<{ ok: boolean }>(`/api/models/${modelId}`, { method: 'DELETE' }),
};

// --- Combos（组合别名：多个模型别名按顺序捆绑，路由时依次尝试直到可用）---
export interface Combo {
  id: string;
  name: string;
  enabled: boolean;
  /** 成员模型别名（按尝试顺序） */
  members: string[];
  created_at: number;
  updated_at: number;
}

export const combosApi = {
  list: () => request<Combo[]>('/api/combos'),
  get: (id: string) => request<Combo>(`/api/combos/${id}`),
  create: (data: { name: string; members: string[]; enabled?: boolean }) =>
    request<Combo>('/api/combos', { method: 'POST', body: data }),
  update: (id: string, data: { name?: string; members?: string[]; enabled?: boolean }) =>
    request<Combo>(`/api/combos/${id}`, { method: 'PUT', body: data }),
  delete: (id: string) => request<{ status: string }>(`/api/combos/${id}`, { method: 'DELETE' }),
};

// --- Stats ---
export interface StatsRow {
  key_id: string;
  key_label?: string;
  prompt_tokens: number;
  completion_tokens: number;
  cache_read_input_tokens: number;
  total_tokens: number;
  requests: number;
  day: string;
}
export interface TopModel {
  model_id: string;
  model_name: string;
  prompt_tokens: number;
  completion_tokens: number;
  cache_read_input_tokens: number;
  total_tokens: number;
  requests: number;
}
export const statsApi = {
  query: (params: { from: number; to: number; granularity?: 'hour' | 'day'; tz_offset: number }) =>
    request<{ data: StatsRow[]; top_model: TopModel | null }>(`/api/stats?from=${params.from}&to=${params.to}&tz_offset=${params.tz_offset}${params.granularity ? `&granularity=${params.granularity}` : ''}`),
};

// --- Request log (paged, newest first) ---
export interface RequestLogRow {
  id: number;
  timestamp: number;
  provider_name: string;
  model_display_name: string;
  service_key_name: string;
  service_key_masked: string;
  request_type: string;
  prompt_tokens: number;
  completion_tokens: number;
  latency_ms: number;
  success: boolean;
  error_message: string | null;
}

export const requestLogApi = {
  page: (params: { page?: number; page_size?: number }) =>
    request<{ total: number; page: number; page_size: number; data: RequestLogRow[] }>(
      `/api/stats/requests?page=${params.page ?? 1}&page_size=${params.page_size ?? 10}`),
};

// --- App Settings ---
export interface AppSettings {
  mcp_websearch: boolean;
  mcp_webfetch: boolean;
  mcp_vision: boolean;
  mcp_vision_provider: string;
  mcp_vision_model: string;
  failover_enabled: boolean;
  theme: string;
  hue: number;
  locale: string;
}

export const settingsApi = {
  get: () => request<AppSettings>('/api/settings'),
  update: (data: { mcp_websearch?: boolean; mcp_webfetch?: boolean; mcp_vision?: boolean; mcp_vision_provider?: string; mcp_vision_model?: string; failover_enabled?: boolean; theme?: string; hue?: number; locale?: string }) =>
    request<{ status: string }>('/api/settings', { method: 'PUT', body: data }),
};

// --- UI Settings (public, for LAN install page) ---
export interface UiSettings {
  theme: string;
  hue: number;
  locale: string;
}

export const uiSettingsApi = {
  get: () => request<UiSettings>('/api/ui-settings'),
};

// --- Data (import / export / reset) ---
export const dataApi = {
  export: () => request<string>('/api/data/export'),
  import: (sql: string) => request<{ status: string }>('/api/data/import', { method: 'POST', body: { sql } }),
  reset: () => request<{ status: string }>('/api/data/reset', { method: 'POST' }),
};

// --- Install（局域网分发）---
export const installApi = {
  localIp: () => request<{ ip: string | null; port: number }>('/api/install/local-ip'),
};
