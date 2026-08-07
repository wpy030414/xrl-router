export const BASE_URL = 'http://localhost:19068';

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
    return text ? JSON.parse(text) : ({} as T);
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
  kind: 'openai' | 'anthropic';
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

// --- Public API ---
export const publicApi = {
  health: () => request<any>('/health'),
};

// --- App Settings ---
export const settingsApi = {
  get: () => request<{ websearch_hijack: boolean; failover_enabled: boolean }>('/api/settings'),
  update: (data: { websearch_hijack?: boolean; failover_enabled?: boolean }) =>
    request<{ status: string }>('/api/settings', { method: 'PUT', body: data }),
};

// --- Data (import / export / reset) ---
export const dataApi = {
  export: () => request<string>('/api/data/export'),
  import: (sql: string) => request<{ status: string }>('/api/data/import', { method: 'POST', body: { sql } }),
  reset: () => request<{ status: string }>('/api/data/reset', { method: 'POST' }),
};

// --- Install（局域网分发）---
export const installApi = {
  localIp: () => request<{ ip: string | null }>('/api/install/local-ip'),
};
