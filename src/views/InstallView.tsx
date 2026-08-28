import { useState, useEffect } from 'react';
import { useSearchParams } from 'react-router';
import { Download, Copy, Check, Loader2, Monitor, Laptop, Smartphone } from 'lucide-react';
import { Button } from '@/components/ui/button';
import { installApi } from '@/lib/api';
import { useT } from '@/i18n';
import { cn } from '@/lib/utils';

type Platform = 'macos' | 'windows' | 'linux';

const PLATFORMS: { key: Platform; label: string; icon: typeof Monitor }[] = [
  { key: 'macos', label: 'macOS', icon: Laptop },
  { key: 'windows', label: 'Windows', icon: Monitor },
  { key: 'linux', label: 'Linux', icon: Monitor },
];

/** 检测当前操作系统 */
function detectPlatform(): Platform {
  if (typeof navigator === 'undefined') return 'macos';
  const ua = navigator.userAgent.toLowerCase();
  if (ua.includes('mac')) return 'macos';
  if (ua.includes('win')) return 'windows';
  return 'linux';
}

export function InstallView() {
  const t = useT();
  const [searchParams] = useSearchParams();

  const apiKey = searchParams.get('key');
  const [platform, setPlatform] = useState<Platform>(detectPlatform());
  const [localIp, setLocalIp] = useState<string>('');
  const [loading, setLoading] = useState(true);
  const [copied, setCopied] = useState(false);

  // Load local IP
  useEffect(() => {
    const load = async () => {
      setLoading(true);
      try {
        const result = await installApi.localIp();
        setLocalIp(result.ip || window.location.hostname);
      } catch (e) {
        console.error('Failed to load local IP:', e);
        setLocalIp(window.location.hostname);
      } finally {
        setLoading(false);
      }
    };
    load();
  }, []);

  // Generate install command
  const installCommand = (() => {
    if (!apiKey) return '';

    const endpoint = `http://${localIp}:19068`;

    switch (platform) {
      case 'macos':
      case 'linux':
        return `curl -fsSL ${endpoint}/install.sh | bash -s -- --key ${apiKey}`;
      case 'windows':
        return `powershell -Command "Invoke-WebRequest -Uri ${endpoint}/install.ps1 -OutFile install.ps1; .\\install.ps1 -Key ${apiKey}"`;
    }
  })();

  // Deploy link
  const deployLink = `${window.location.origin}/install?key=${apiKey}`;

  const handleCopyCommand = async () => {
    await navigator.clipboard.writeText(installCommand);
    setCopied(true);
    setTimeout(() => setCopied(false), 2000);
  };

  const handleCopyLink = async () => {
    await navigator.clipboard.writeText(deployLink);
    setCopied(true);
    setTimeout(() => setCopied(false), 2000);
  };

  if (loading) {
    return (
      <div className="flex items-center justify-center min-h-screen">
        <Loader2 className="w-6 h-6 animate-spin text-muted-foreground" />
      </div>
    );
  }

  return (
    <div className="min-h-screen bg-background flex items-center justify-center p-6">
      <div className="w-full max-w-2xl space-y-8">
        {/* Header */}
        <div className="text-center space-y-3">
          <div className="flex justify-center">
            <div className="w-16 h-16 rounded-full bg-primary/10 flex items-center justify-center">
              <Download className="w-8 h-8 text-primary" />
            </div>
          </div>
          <h1 className="text-4xl font-bold">{t('install.title')}</h1>
          <p className="text-muted-foreground">
            {t('install.subtitle')}
          </p>
        </div>

        {/* No API key warning */}
        {!apiKey && (
          <div className="rounded-lg border border-yellow-500/50 bg-yellow-500/10 p-6 text-center space-y-2">
            <h3 className="text-lg font-semibold text-yellow-600 dark:text-yellow-400">
              {t('install.no_key_title')}
            </h3>
            <p className="text-sm text-muted-foreground">
              {t('install.no_key_desc')}
            </p>
          </div>
        )}

        {apiKey && (
          <>
            {/* Platform selector */}
            <div className="space-y-3">
              <label className="text-sm font-medium">{t('install.platform_label')}</label>
              <div className="flex gap-3">
                {PLATFORMS.map((p) => {
                  const Icon = p.icon;
                  return (
                    <Button
                      key={p.key}
                      variant={platform === p.key ? 'default' : 'outline'}
                      className="flex-1"
                      onClick={() => setPlatform(p.key)}
                    >
                      <Icon className="w-4 h-4 mr-2" />
                      {p.label}
                    </Button>
                  );
                })}
              </div>
            </div>

            {/* Install command */}
            <div className="space-y-3">
              <label className="text-sm font-medium">{t('install.command_label')}</label>
              <div className="relative">
                <pre className="rounded-lg border bg-muted p-4 pr-12 overflow-x-auto">
                  <code className="text-sm font-mono">{installCommand}</code>
                </pre>
                <Button
                  size="icon"
                  variant="ghost"
                  className="absolute top-2 right-2"
                  onClick={handleCopyCommand}
                >
                  {copied ? (
                    <Check className="w-4 h-4 text-green-500" />
                  ) : (
                    <Copy className="w-4 h-4" />
                  )}
                </Button>
              </div>
              <p className="text-xs text-muted-foreground">
                {t('install.command_hint')}
              </p>
            </div>

            {/* Deploy link */}
            <div className="space-y-3">
              <label className="text-sm font-medium">{t('install.deploy_link_label')}</label>
              <div className="relative">
                <input
                  type="text"
                  readOnly
                  value={deployLink}
                  className="w-full rounded-lg border bg-muted px-4 py-2 pr-12 text-sm font-mono"
                />
                <Button
                  size="icon"
                  variant="ghost"
                  className="absolute top-1/2 -translate-y-1/2 right-2"
                  onClick={handleCopyLink}
                >
                  {copied ? (
                    <Check className="w-4 h-4 text-green-500" />
                  ) : (
                    <Copy className="w-4 h-4" />
                  )}
                </Button>
              </div>
              <p className="text-xs text-muted-foreground">
                {t('install.deploy_link_hint')}
              </p>
            </div>

            {/* Instructions */}
            <div className="rounded-lg border bg-card p-6 space-y-4">
              <h3 className="text-lg font-semibold">{t('install.instructions_title')}</h3>
              <ol className="space-y-3 text-sm">
                <li className="flex gap-3">
                  <span className="flex-shrink-0 w-6 h-6 rounded-full bg-primary/10 text-primary flex items-center justify-center text-xs font-bold">
                    1
                  </span>
                  <span>{t('install.step1')}</span>
                </li>
                <li className="flex gap-3">
                  <span className="flex-shrink-0 w-6 h-6 rounded-full bg-primary/10 text-primary flex items-center justify-center text-xs font-bold">
                    2
                  </span>
                  <span>{t('install.step2')}</span>
                </li>
                <li className="flex gap-3">
                  <span className="flex-shrink-0 w-6 h-6 rounded-full bg-primary/10 text-primary flex items-center justify-center text-xs font-bold">
                    3
                  </span>
                  <span>{t('install.step3')}</span>
                </li>
              </ol>
            </div>
          </>
        )}
      </div>
    </div>
  );
}

export default InstallView;
