import { useState, useEffect, useMemo } from 'react';
import {
  Palette,
  Globe,
  Moon,
  Sun,
  Monitor,
  Power,
  Search,
  FileText,
  Bell,
  Download,
  Upload,
  RotateCcw,
  Copy,
  Check,
  Loader2,
  Zap,
  Info,
  Database,
} from 'lucide-react';
import { Button } from '@/components/ui/button';
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog';
import { useSettingsStore } from '@/stores/settings';
import { useTheme } from '@/hooks/useTheme';
import { useI18nStore, type Locale } from '@/i18n';
import { useT } from '@/i18n';
import { settingsApi, type AppSettings } from '@/lib/api';
import { tauriAutostart, tauriDialog, tauriFs, tauriShell, tauriApp, isTauri } from '@/lib/tauri';
import { cn } from '@/lib/utils';

type Tab = 'general' | 'routing' | 'data';

const TABS: { key: Tab; labelKey: string }[] = [
  { key: 'general', labelKey: 'settings.tab.general' },
  { key: 'routing', labelKey: 'settings.tab.routing' },
  { key: 'data', labelKey: 'settings.tab.data' },
];

export function SettingsView() {
  const t = useT();
  const { theme, hue, setTheme, setHue } = useTheme();
  const { locale, setLocale } = useI18nStore();
  const { settings, fetchSettings, updateSettings, loading } = useSettingsStore();

  const [activeTab, setActiveTab] = useState<Tab>('general');
  const [autostartEnabled, setAutostartEnabled] = useState(false);
  const [version, setVersion] = useState<string>('');
  const [saving, setSaving] = useState(false);
  const [resetDialogOpen, setResetDialogOpen] = useState(false);
  const [importDialogOpen, setImportDialogOpen] = useState(false);

  // Load settings and Tauri state
  useEffect(() => {
    const load = async () => {
      await fetchSettings();
      if (isTauri()) {
        const autoEnabled = await tauriAutostart.isEnabled();
        setAutostartEnabled(!!autoEnabled);
        const ver = await tauriApp.getVersion();
        if (ver) setVersion(ver);
      }
    };
    load();
  }, []);

  // Toggle handlers
  const handleSettingToggle = async (key: keyof AppSettings, value: boolean) => {
    setSaving(true);
    try {
      await updateSettings({ [key]: value });
    } finally {
      setSaving(false);
    }
  };

  const handleAutostartToggle = async () => {
    const newValue = !autostartEnabled;
    setAutostartEnabled(newValue);
    if (newValue) {
      await tauriAutostart.enable();
    } else {
      await tauriAutostart.disable();
    }
  };

  const handleThemeChange = async (newTheme: 'system' | 'light' | 'dark') => {
    setTheme(newTheme);
    await updateSettings({ theme: newTheme });
  };

  const handleHueChange = async (newHue: number) => {
    setHue(newHue);
    await updateSettings({ hue: newHue });
  };

  const handleLocaleChange = async (newLocale: Locale) => {
    setLocale(newLocale);
    await updateSettings({ locale: newLocale });
  };

  // Export
  const handleExport = async () => {
    try {
      const resp = await fetch('/api/data/export');
      if (!resp.ok) throw new Error('Export failed');
      const sql = await resp.text();

      if (isTauri()) {
        const path = await tauriDialog.save({
          title: 'Export Data',
          defaultPath: `xrl-router-backup-${new Date().toISOString().split('T')[0]}.sql`,
          filters: [{ name: 'SQL', extensions: ['sql'] }],
        });
        if (path) {
          await tauriFs.writeTextFile(path, sql);
        }
      } else {
        const blob = new Blob([sql], { type: 'text/plain' });
        const url = URL.createObjectURL(blob);
        const a = document.createElement('a');
        a.href = url;
        a.download = `xrl-router-backup-${new Date().toISOString().split('T')[0]}.sql`;
        a.click();
        URL.revokeObjectURL(url);
      }
    } catch (e) {
      console.error('Export failed:', e);
    }
  };

  // Import
  const handleImport = async () => {
    try {
      let sql: string | null = null;
      if (isTauri()) {
        const path = await tauriDialog.open({
          title: 'Import Data',
          filters: [{ name: 'SQL', extensions: ['sql'] }],
        });
        if (path) {
          sql = await tauriFs.readTextFile(path);
        }
      } else {
        const input = document.createElement('input');
        input.type = 'file';
        input.accept = '.sql';
        input.onchange = async (e) => {
          const file = (e.target as HTMLInputElement).files?.[0];
          if (file) {
            sql = await file.text();
            await doImport(sql!);
          }
        };
        input.click();
        return;
      }
      if (sql) {
        await doImport(sql);
      }
    } catch (e) {
      console.error('Import failed:', e);
    }
    setImportDialogOpen(false);
  };

  const doImport = async (sql: string) => {
    const resp = await fetch('/api/data/import', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ sql }),
    });
    if (!resp.ok) throw new Error('Import failed');
    window.location.reload();
  };

  // Reset
  const handleReset = async () => {
    try {
      await fetch('/api/data/reset', { method: 'POST' });
      window.location.reload();
    } catch (e) {
      console.error('Reset failed:', e);
    }
    setResetDialogOpen(false);
  };

  // MCP endpoint info — always local, use 127.0.0.1 to avoid Tauri's tauri.localhost hostname
  const mcpEndpoint = 'http://127.0.0.1:19068/mcp';
  const mcpCmd = `claude mcp add --scope user --transport http xrl-router ${mcpEndpoint} --header "Authorization: Bearer <SERVICE_KEY>"`;
  const [copied, setCopied] = useState<'endpoint' | 'cmd' | null>(null);

  const handleCopyMcp = async (text: string, which: 'endpoint' | 'cmd') => {
    await navigator.clipboard.writeText(text);
    setCopied(which);
    setTimeout(() => setCopied(null), 2000);
  };

  // 令牌色色条：与 applyHue 同源（hsl(h 70% L)，L 取当前主题实际生效的亮度），
  // 每 30° 一个停靠点 —— 滑杆位置 h 处的颜色即真实应用的主题色。
  const hueBarLightness = useMemo(() => {
    const v = document.documentElement.style.getPropertyValue('--primary') || '';
    const l = v.trim().split(/\s+/)[2];
    return v.includes('70%') && /^\d+(\.\d+)?%$/.test(l || '') ? l : '45%';
  }, [theme]);
  const hueBar = `linear-gradient(to right, ${Array.from(
    { length: 13 },
    (_, i) => `hsl(${(i * 30) % 360} 70% ${hueBarLightness})`
  ).join(', ')})`;

  return (
    <div className="space-y-6">
      {/* Header */}
      <div className="flex justify-between items-start gap-4 flex-wrap">
        <h2 className="text-3xl font-normal m-0">{t('settings.title')}</h2>
      </div>

      {/* Tabs */}
      <div className="flex gap-2 border-b pb-2">
        {TABS.map((tab) => (
          <Button
            key={tab.key}
            variant={activeTab === tab.key ? 'default' : 'ghost'}
            size="sm"
            onClick={() => setActiveTab(tab.key)}
          >
            {t(tab.labelKey)}
          </Button>
        ))}
      </div>

      {/* General Tab */}
      {activeTab === 'general' && (
        <div className="space-y-6">
          {/* About */}
          <section className="space-y-2">
            <div className="flex items-center gap-2">
              <Info className="w-5 h-5" />
              <h3 className="text-lg font-semibold">{t('settings.about.title')}</h3>
            </div>
            {version && (
              <p className="text-sm text-muted-foreground">
                {t('settings.about.version', { version })}
              </p>
            )}
            <p className="text-sm text-muted-foreground">{t('settings.about.desc')}</p>
            <a
              href="https://github.com/wpy030414/xrl-router"
              target="_blank"
              rel="noopener noreferrer"
              className="text-sm text-primary hover:underline"
            >
              {t('settings.about.github')}
            </a>
          </section>

          {/* Language */}
          <section className="space-y-3">
            <div className="flex items-center gap-2">
              <Globe className="w-5 h-5" />
              <h3 className="text-lg font-semibold">{t('settings.language.title')}</h3>
            </div>
            <p className="text-sm text-muted-foreground">{t('settings.language.desc')}</p>
            <div className="flex gap-2">
              <Button
                variant={locale === 'zh-CN' ? 'default' : 'outline'}
                size="sm"
                onClick={() => handleLocaleChange('zh-CN')}
              >
                {t('settings.language.zh-CN')}
              </Button>
              <Button
                variant={locale === 'en' ? 'default' : 'outline'}
                size="sm"
                onClick={() => handleLocaleChange('en')}
              >
                {t('settings.language.en')}
              </Button>
            </div>
          </section>

          {/* Theme */}
          <section className="space-y-3">
            <div className="flex items-center gap-2">
              <Palette className="w-5 h-5" />
              <h3 className="text-lg font-semibold">{t('settings.theme.title')}</h3>
            </div>
            <p className="text-sm text-muted-foreground">{t('settings.theme.desc')}</p>
            <div className="flex gap-2 flex-wrap">
              <Button
                variant={theme === 'system' ? 'default' : 'outline'}
                size="sm"
                onClick={() => handleThemeChange('system')}
              >
                <Monitor className="w-4 h-4 mr-1" />
                {t('settings.theme.system')}
              </Button>
              <Button
                variant={theme === 'light' ? 'default' : 'outline'}
                size="sm"
                onClick={() => handleThemeChange('light')}
              >
                <Sun className="w-4 h-4 mr-1" />
                {t('settings.theme.light')}
              </Button>
              <Button
                variant={theme === 'dark' ? 'default' : 'outline'}
                size="sm"
                onClick={() => handleThemeChange('dark')}
              >
                <Moon className="w-4 h-4 mr-1" />
                {t('settings.theme.dark')}
              </Button>
            </div>

            {/* Hue slider */}
            <div className="space-y-2 pt-2">
              <label className="text-sm font-medium">{t('settings.theme.hue')}</label>
              <div className="flex items-center gap-3">
                {/* 圆角色条画在独立轨道层，input 透明浮在其上（保留滑杆交互）。
                    包装层用 flex 居中 input，避免 inline 行框基线留白造成的 3px 偏移 */}
                <div className="relative flex-1 flex items-center">
                  <div
                    aria-hidden
                    className="absolute inset-x-0 top-1/2 h-2 -translate-y-1/2 rounded-full"
                    style={{ background: hueBar }}
                  />
                  <input
                    type="range"
                    min="0"
                    max="360"
                    value={hue}
                    onChange={(e) => handleHueChange(parseInt(e.target.value))}
                    className="hue-slider relative w-full cursor-pointer"
                  />
                </div>
                <span className="text-sm font-mono w-10 text-right">{hue}</span>
                <Button
                  variant="ghost"
                  size="sm"
                  onClick={() => handleHueChange(200)}
                >
                  {t('settings.theme.hue_reset')}
                </Button>
              </div>
            </div>
          </section>

          {/* Autostart (Tauri only) */}
          {isTauri() && (
            <section className="space-y-3">
              <div className="flex items-center gap-2">
                <Power className="w-5 h-5" />
                <h3 className="text-lg font-semibold">{t('settings.autostart.title')}</h3>
              </div>
              <p className="text-sm text-muted-foreground">{t('settings.autostart.desc')}</p>
              <button
                onClick={handleAutostartToggle}
                className={cn(
                  'relative inline-flex h-6 w-11 items-center rounded-full transition-colors',
                  autostartEnabled ? 'bg-primary' : 'bg-muted-foreground/20'
                )}
              >
                <span
                  className={cn(
                    'inline-block h-4 w-4 transform rounded-full bg-white transition-transform',
                    autostartEnabled ? 'translate-x-6' : 'translate-x-1'
                  )}
                />
              </button>
              <span className="text-sm ml-2">
                {autostartEnabled ? t('settings.autostart.on') : t('settings.autostart.off')}
              </span>
            </section>
          )}
        </div>
      )}

      {/* Routing Tab */}
      {activeTab === 'routing' && settings && (
        <div className="space-y-6">
          {/* Failover */}
          <section className="space-y-3">
            <div className="flex items-center gap-2">
              <Zap className="w-5 h-5" />
              <h3 className="text-lg font-semibold">{t('settings.failover.title')}</h3>
            </div>
            <p className="text-sm text-muted-foreground">{t('settings.failover.desc')}</p>
            <button
              onClick={() => handleSettingToggle('failover_enabled', !settings.failover_enabled)}
              className={cn(
                'relative inline-flex h-6 w-11 items-center rounded-full transition-colors',
                settings.failover_enabled ? 'bg-primary' : 'bg-muted-foreground/20'
              )}
            >
              <span
                className={cn(
                  'inline-block h-4 w-4 transform rounded-full bg-white transition-transform',
                  settings.failover_enabled ? 'translate-x-6' : 'translate-x-1'
                )}
              />
            </button>
            <span className="text-sm ml-2">
              {settings.failover_enabled ? t('settings.failover.on') : t('settings.failover.off')}
            </span>
          </section>

          {/* MCP Connection Info（在 MCP Tools 之上：先接入、再开关工具） */}
          <section className="space-y-3">
            <h3 className="text-lg font-semibold">{t('settings.mcp_info.title')}</h3>
            <p className="text-sm text-muted-foreground">{t('settings.mcp_info.desc')}</p>
            <div className="space-y-2">
              <label className="text-sm font-medium">{t('settings.mcp_info.endpoint')}</label>
              <div className="flex gap-2">
                <input
                  type="text"
                  readOnly
                  value={mcpEndpoint}
                  className="flex-1 px-3 py-2 rounded-md border border-input bg-background text-sm font-mono"
                />
                <Button size="sm" onClick={() => handleCopyMcp(mcpEndpoint, 'endpoint')}>
                  {copied === 'endpoint' ? <Check className="w-4 h-4" /> : <Copy className="w-4 h-4" />}
                </Button>
              </div>
              {/* 注册命令与端点同等待遇：独立输入框 + 一键复制。
                  label 默认 inline，垂直 padding 不参与布局——须 block 才真正把间距撑开 */}
              <label className="block text-sm font-medium">{t('settings.mcp_info.register')}</label>
              <div className="flex gap-2">
                <input
                  type="text"
                  readOnly
                  value={mcpCmd}
                  className="flex-1 px-3 py-2 rounded-md border border-input bg-background text-sm font-mono"
                />
                <Button size="sm" onClick={() => handleCopyMcp(mcpCmd, 'cmd')}>
                  {copied === 'cmd' ? <Check className="w-4 h-4" /> : <Copy className="w-4 h-4" />}
                </Button>
              </div>
            </div>
          </section>

          {/* MCP Tools */}
          <section className="space-y-3">
            <h3 className="text-lg font-semibold">MCP Tools</h3>

            {/* Web Search */}
            <div className="flex items-center justify-between py-3 border-b">
              <div className="flex items-center gap-2">
                <Search className="w-5 h-5" />
                <div>
                  <p className="font-medium">{t('settings.mcp_websearch.title')}</p>
                  <p className="text-sm text-muted-foreground">{t('settings.mcp_websearch.desc')}</p>
                </div>
              </div>
              <button
                onClick={() => handleSettingToggle('mcp_websearch', !settings.mcp_websearch)}
                className={cn(
                  'relative inline-flex h-6 w-11 items-center rounded-full transition-colors',
                  settings.mcp_websearch ? 'bg-primary' : 'bg-muted-foreground/20'
                )}
              >
                <span
                  className={cn(
                    'inline-block h-4 w-4 transform rounded-full bg-white transition-transform',
                    settings.mcp_websearch ? 'translate-x-6' : 'translate-x-1'
                  )}
                />
              </button>
            </div>

            {/* Web Fetch */}
            <div className="flex items-center justify-between py-3 border-b">
              <div className="flex items-center gap-2">
                <FileText className="w-5 h-5" />
                <div>
                  <p className="font-medium">{t('settings.mcp_webfetch.title')}</p>
                  <p className="text-sm text-muted-foreground">{t('settings.mcp_webfetch.desc')}</p>
                </div>
              </div>
              <button
                onClick={() => handleSettingToggle('mcp_webfetch', !settings.mcp_webfetch)}
                className={cn(
                  'relative inline-flex h-6 w-11 items-center rounded-full transition-colors',
                  settings.mcp_webfetch ? 'bg-primary' : 'bg-muted-foreground/20'
                )}
              >
                <span
                  className={cn(
                    'inline-block h-4 w-4 transform rounded-full bg-white transition-transform',
                    settings.mcp_webfetch ? 'translate-x-6' : 'translate-x-1'
                  )}
                />
              </button>
            </div>

            {/* Notify */}
            <div className="flex items-center justify-between py-3 border-b">
              <div className="flex items-center gap-2">
                <Bell className="w-5 h-5" />
                <div>
                  <p className="font-medium">{t('settings.mcp_notify.title')}</p>
                  <p className="text-sm text-muted-foreground">{t('settings.mcp_notify.desc')}</p>
                </div>
              </div>
              <button
                onClick={() => handleSettingToggle('mcp_notify', !settings.mcp_notify)}
                className={cn(
                  'relative inline-flex h-6 w-11 items-center rounded-full transition-colors',
                  settings.mcp_notify ? 'bg-primary' : 'bg-muted-foreground/20'
                )}
              >
                <span
                  className={cn(
                    'inline-block h-4 w-4 transform rounded-full bg-white transition-transform',
                    settings.mcp_notify ? 'translate-x-6' : 'translate-x-1'
                  )}
                />
              </button>
            </div>
          </section>
        </div>
      )}

      {/* Privacy Tab */}
      {activeTab === 'data' && (
        <div className="space-y-6">
          {/* Export/Import/Reset */}
          <section className="space-y-3">
            <div className="flex items-center gap-2">
              <Database className="w-5 h-5" />
              <h3 className="text-lg font-semibold">{t('settings.data.title')}</h3>
            </div>
            <p className="text-sm text-muted-foreground">{t('settings.data.desc')}</p>
            <div className="flex gap-3">
              <Button variant="outline" onClick={handleExport}>
                <Download className="w-4 h-4 mr-2" />
                {t('settings.data.export.button')}
              </Button>
              <Button onClick={() => setImportDialogOpen(true)}>
                <Upload className="w-4 h-4 mr-2" />
                {t('settings.data.import.button')}
              </Button>
              <Button variant="destructive" onClick={() => setResetDialogOpen(true)}>
                <RotateCcw className="w-4 h-4 mr-2" />
                {t('settings.data.reset.button')}
              </Button>
            </div>
          </section>
        </div>
      )}

      {/* Reset Confirmation Dialog */}
      <Dialog open={resetDialogOpen} onOpenChange={setResetDialogOpen}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>{t('settings.data.reset.title')}</DialogTitle>
            <DialogDescription>{t('settings.data.reset.confirm')}</DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button variant="outline" onClick={() => setResetDialogOpen(false)}>
              {t('settings.cancel')}
            </Button>
            <Button variant="destructive" onClick={handleReset}>
              {t('settings.confirm')}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      {/* Import Confirmation Dialog */}
      <Dialog open={importDialogOpen} onOpenChange={setImportDialogOpen}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>{t('settings.data.import.button')}</DialogTitle>
            <DialogDescription>{t('settings.data.import.confirm')}</DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button variant="outline" onClick={() => setImportDialogOpen(false)}>
              {t('settings.cancel')}
            </Button>
            <Button onClick={handleImport}>
              {t('settings.confirm')}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}

export default SettingsView;
