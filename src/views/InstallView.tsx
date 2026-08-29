import { useState, useEffect, useMemo, useRef } from 'react';
import { useSearchParams } from 'react-router';
import { Download, Copy, Check, Loader2, Monitor, Laptop, User, Brain, Terminal } from 'lucide-react';
import { Button } from '@/components/ui/button';
import { uiSettingsApi } from '@/lib/api';
import { useT, useI18nStore } from '@/i18n';
import { useThemeStore } from '@/hooks/useTheme';
import { cn } from '@/lib/utils';

type Platform = 'macos' | 'windows';
type Consumer = 'claude-code' | 'chatgpt';

interface ModelItem {
  id: string;
  owned_by: string;
}

const SLOTS = ['FABLE', 'HAIKU', 'OPUS', 'SONNET'] as const;

function detectPlatform(): Platform {
  // 默认 macOS
  if (typeof navigator === 'undefined') return 'macos';
  return /Windows/i.test(navigator.userAgent) ? 'windows' : 'macos';
}

// 兼容不安全上下文的复制（HTTP 环境 navigator.clipboard 不可用）
function copyToClipboard(text: string): boolean {
  const textarea = document.createElement('textarea');
  textarea.value = text;
  textarea.style.position = 'fixed';
  textarea.style.opacity = '0';
  document.body.appendChild(textarea);
  textarea.select();
  try {
    return document.execCommand('copy');
  } catch {
    return false;
  } finally {
    document.body.removeChild(textarea);
  }
}

function q(v: string): string {
  return `'${v}'`;
}

// ── Command generation ──

function envModelLines(model: string): string[] {
  const lines: string[] = [];
  for (const slot of SLOTS) {
    lines.push(`ANTHROPIC_DEFAULT_${slot}_MODEL=${q(model)}`);
    lines.push(`ANTHROPIC_DEFAULT_${slot}_MODEL_NAME=${q(model)}`);
  }
  lines.push(`CLAUDE_CODE_SUBAGENT_MODEL=${q(model)}`);
  return lines;
}

function buildClaudeCodeBash(token: string, base: string, model: string): string {
  const envAssignments = [
    `j.env.ANTHROPIC_AUTH_TOKEN=${q(token)};`,
    `j.env.ANTHROPIC_BASE_URL=${q(base)};`,
  ];
  if (model) {
    for (const line of envModelLines(model)) {
      const [k, ...rest] = line.split('=');
      envAssignments.push(`j.env.${k}=${rest.join('=')};`);
    }
  }
  const nodeScript =
    `const fs=require('fs'),p=process.env.HOME+'/.claude/settings.json';` +
    `let j={};try{j=JSON.parse(fs.readFileSync(p))}catch{};` +
    `j.env=j.env||{};` +
    envAssignments.join('') +
    `fs.writeFileSync(p,JSON.stringify(j,null,2))`;
  return `mkdir -p ~/.claude && node -e "${nodeScript}"`;
}

function buildClaudeCodePS(token: string, base: string, model: string): string {
  const parts = [
    '$p="$env:USERPROFILE\\.claude\\settings.json"',
    'New-Item -ItemType Directory -Force "$env:USERPROFILE\\.claude" | Out-Null',
    '$j=@{}; if(Test-Path $p){ $j=Get-Content $p -Raw | ConvertFrom-Json }',
    'if(-not $j.env){ $j.env=@{} }',
    `$j.env.ANTHROPIC_AUTH_TOKEN='${token}'`,
    `$j.env.ANTHROPIC_BASE_URL='${base}'`,
  ];
  if (model) {
    for (const line of envModelLines(model)) {
      const [k, ...rest] = line.split('=');
      parts.push(`$j.env.${k}=${rest.join('=')}`);
    }
  }
  parts.push('$j | ConvertTo-Json -Depth 10 | Set-Content $p');
  return parts.join('; ');
}

function buildChatGPTBash(token: string, base: string, model: string): string {
  const toml = [
    `model = "${model}"`,
    `model_provider = "xrl"`,
    ``,
    `[model_providers.xrl]`,
    `name = "XRL Router"`,
    `base_url = "${base}/v1"`,
  ].join('\n');
  return (
    `mkdir -p ~/.codex && cat > ~/.codex/config.toml << 'CODEX_EOF'\n` +
    toml +
    `\nCODEX_EOF\n` +
    `printf '{"OPENAI_API_KEY":"${token}"}\\n' > ~/.codex/auth.json`
  );
}

function buildChatGPTPS(token: string, base: string, model: string): string {
  const tomlLines = [
    `model = '${model}'`,
    `model_provider = 'xrl'`,
    ``,
    `[model_providers.xrl]`,
    `name = 'XRL Router'`,
    `base_url = '${base}/v1'`,
  ].join('`n');
  return [
    '$d="$env:USERPROFILE\\.codex"',
    'New-Item -ItemType Directory -Force $d | Out-Null',
    `Set-Content "$d\\config.toml" "${tomlLines}"`,
    `Set-Content "$d\\auth.json" '{"OPENAI_API_KEY":"${token}"}'`,
  ].join('; ');
}

function buildCommand(
  consumer: Consumer,
  platform: Platform,
  token: string,
  base: string,
  model: string,
): string {
  switch (consumer) {
    case 'claude-code':
      return platform === 'windows'
        ? buildClaudeCodePS(token, base, model)
        : buildClaudeCodeBash(token, base, model);
    case 'chatgpt':
      return platform === 'windows'
        ? buildChatGPTPS(token, base, model)
        : buildChatGPTBash(token, base, model);
  }
}

export function InstallView() {
  const t = useT();
  const [searchParams] = useSearchParams();

  const apiKey = searchParams.get('key') || '';
  const base = window.location.origin;

  const [platform, setPlatform] = useState<Platform>(detectPlatform());
  const [consumer, setConsumer] = useState<Consumer>('claude-code');
  const [models, setModels] = useState<ModelItem[]>([]);
  const [selectedModel, setSelectedModel] = useState('');
  const [modelsLoading, setModelsLoading] = useState(true);
  const [modelsError, setModelsError] = useState('');
  const [copied, setCopied] = useState(false);

  // Sync UI settings from host (theme/hue/locale)
  useEffect(() => {
    const loadUi = async () => {
      try {
        const settings = await uiSettingsApi.get();
        if (settings.theme) {
          useThemeStore.getState().setTheme(settings.theme as 'light' | 'dark' | 'system');
        }
        if (typeof settings.hue === 'number') {
          useThemeStore.getState().setHue(settings.hue);
        }
        if (settings.locale === 'zh-CN' || settings.locale === 'en') {
          useI18nStore.getState().setLocale(settings.locale);
        }
      } catch {
        // fallback: URL ?lang= or browser language
        const urlLang = searchParams.get('lang');
        if (urlLang === 'zh-CN' || urlLang === 'en') {
          useI18nStore.getState().setLocale(urlLang);
        }
      }
    };
    loadUi();
  }, [searchParams]);

  // Fetch models from gateway
  const tRef = useRef(t);
  tRef.current = t;

  useEffect(() => {
    if (!apiKey) return;
    const fetchModels = async () => {
      setModelsLoading(true);
      setModelsError('');
      try {
        const r = await fetch(`${base}/v1/models`, {
          headers: { 'x-api-key': apiKey },
        });
        if (!r.ok) throw new Error('HTTP ' + r.status);
        const data = await r.json();
        const list = (data?.data || []) as { id: string; owned_by?: string }[];
        setModels(list.map((m) => ({ id: m.id, owned_by: m.owned_by || '' })));
        if (list.length) setSelectedModel(list[0].id);
      } catch (e: unknown) {
        const msg = e instanceof Error ? e.message : String(e);
        setModelsError(tRef.current('install.models_fetch_error', { msg }));
      } finally {
        setModelsLoading(false);
      }
    };
    fetchModels();
  }, [apiKey, base]);

  const command = useMemo(
    () => buildCommand(consumer, platform, apiKey, base, selectedModel),
    [consumer, platform, apiKey, base, selectedModel],
  );

  const platformLabel = platform === 'windows' ? t('install.platform_windows_powershell') : t('install.platform_macos_bash');

  const handleCopy = async () => {
    const ok = copyToClipboard(command);
    if (ok) {
      setCopied(true);
      setTimeout(() => setCopied(false), 2000);
    }
  };

  return (
    <div className="min-h-screen bg-background flex flex-col items-center p-8">
      <div className="w-full max-w-3xl space-y-6">
        {/* Header */}
        <div className="space-y-2">
          <h2 className="text-2xl font-bold">{t('install.title')}</h2>
        </div>

        {/* No key placeholder */}
        {!apiKey && (
          <div className="rounded-lg border border-yellow-500/50 bg-yellow-500/10 p-6 text-center space-y-2">
            <h3 className="text-lg font-semibold text-yellow-600 dark:text-yellow-400">
              {t('install.no_key_title')}
            </h3>
            <p className="text-sm text-muted-foreground">{t('install.no_key_desc')}</p>
          </div>
        )}

        {apiKey && (
          <>
            {/* OS selector */}
            <section className="rounded-lg border bg-card p-5 space-y-4">
              <div className="flex items-center gap-3">
                <div className="w-10 h-10 rounded-full bg-muted flex items-center justify-center">
                  <Laptop className="w-5 h-5 text-muted-foreground" />
                </div>
                <h3 className="text-base font-medium">{t('install.platform_label')}</h3>
              </div>
              <div className="flex gap-2">
                {(['macos', 'windows'] as Platform[]).map((p) => (
                  <Button
                    key={p}
                    variant={platform === p ? 'default' : 'outline'}
                    size="sm"
                    onClick={() => setPlatform(p)}
                  >
                    {p === 'macos' ? (
                      <Laptop className="w-4 h-4 mr-1.5" />
                    ) : (
                      <Monitor className="w-4 h-4 mr-1.5" />
                    )}
                    {p === 'macos' ? t('install.platform_macos') : t('install.platform_windows')}
                  </Button>
                ))}
              </div>
            </section>

            {/* Consumer selector */}
            <section className="rounded-lg border bg-card p-5 space-y-4">
              <div className="flex items-center gap-3">
                <div className="w-10 h-10 rounded-full bg-muted flex items-center justify-center">
                  <User className="w-5 h-5 text-muted-foreground" />
                </div>
                <h3 className="text-base font-medium">{t('install.agent_label')}</h3>
              </div>
              <div className="flex gap-2">
                <Button
                  variant={consumer === 'claude-code' ? 'default' : 'outline'}
                  size="sm"
                  onClick={() => setConsumer('claude-code')}
                >
                  {t('install.mode_claude_code')}
                </Button>
                <Button
                  variant={consumer === 'chatgpt' ? 'default' : 'outline'}
                  size="sm"
                  onClick={() => setConsumer('chatgpt')}
                >
                  {t('install.mode_chatgpt')}
                </Button>
              </div>
            </section>

            {/* Model selector */}
            <section className="rounded-lg border bg-card p-5 space-y-4">
              <div className="flex items-center gap-3">
                <div className="w-10 h-10 rounded-full bg-muted flex items-center justify-center">
                  <Brain className="w-5 h-5 text-muted-foreground" />
                </div>
                <h3 className="text-base font-medium">{t('install.model_label')}</h3>
              </div>

              {modelsLoading && (
                <div className="flex items-center gap-2">
                  <Loader2 className="w-4 h-4 animate-spin text-muted-foreground" />
                  <span className="text-sm text-muted-foreground">
                    {t('install.models_loading')}
                  </span>
                </div>
              )}

              {modelsError && !modelsLoading && (
                <div className="space-y-1">
                  <p className="text-sm text-destructive">{modelsError}</p>
                  <p className="text-xs text-muted-foreground">
                    {t('install.models_error_ignore')}
                  </p>
                </div>
              )}

              {!modelsLoading && !modelsError && models.length === 0 && (
                <p className="text-sm text-muted-foreground">{t('install.no_models')}</p>
              )}

              {!modelsLoading && !modelsError && models.length > 0 && (
                <div className="relative">
                  <select
                    value={selectedModel}
                    onChange={(e) => setSelectedModel(e.target.value)}
                    className={cn(
                      'flex h-10 w-full appearance-none rounded-md border border-input bg-background px-3 py-2 pr-9 text-sm',
                      'focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring',
                    )}
                  >
                    {models.map((m) => (
                      <option key={m.id} value={m.id}>
                        {m.id}{m.owned_by ? ` · ${m.owned_by}` : ''}
                      </option>
                    ))}
                  </select>
                  <svg
                    className="pointer-events-none absolute right-3 top-1/2 h-4 w-4 -translate-y-1/2 text-muted-foreground"
                    xmlns="http://www.w3.org/2000/svg"
                    viewBox="0 0 24 24"
                    fill="none"
                    stroke="currentColor"
                    strokeWidth="2"
                    strokeLinecap="round"
                    strokeLinejoin="round"
                  >
                    <path d="m6 9 6 6 6-6" />
                  </svg>
                </div>
              )}
            </section>

            {/* Command output */}
            <section className="rounded-lg border bg-card p-5 space-y-4">
              <div className="flex items-center gap-3">
                <div className="w-10 h-10 rounded-full bg-muted flex items-center justify-center">
                  <Terminal className="w-5 h-5 text-muted-foreground" />
                </div>
                <h3 className="text-base font-medium">
                  {t('install.command_title', { platform: platformLabel })}
                </h3>
              </div>
              <div className="relative">
                <pre className="rounded-lg border bg-muted p-4 pr-12 overflow-x-auto text-sm font-mono whitespace-pre-wrap break-all">
                  {command}
                </pre>
                <Button
                  size="icon"
                  variant="ghost"
                  className="absolute top-2 right-2"
                  onClick={handleCopy}
                >
                  {copied ? (
                    <Check className="w-4 h-4 text-green-500" />
                  ) : (
                    <Copy className="w-4 h-4" />
                  )}
                </Button>
              </div>
            </section>
          </>
        )}
      </div>
    </div>
  );
}

export default InstallView;
