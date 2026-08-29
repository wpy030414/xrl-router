import { Outlet, NavLink, useLocation } from 'react-router';
import { Radio, Cloud, Server, Combine, Key, BarChart3, Settings, PanelLeft, Minus, Square, X } from 'lucide-react';
import { useT } from '@/i18n';
import { ConnectionStatus } from './ConnectionStatus';
import { PluginRegisterDialog } from './PluginRegisterDialog';
import { SystemStatusBar } from './SystemStatusBar';
import { isWindows, getCurrentWindow } from '@/lib/tauri';
import {
  Sidebar,
  SidebarContent,
  SidebarFooter,
  SidebarGroup,
  SidebarHeader,
  SidebarInset,
  SidebarMenu,
  SidebarMenuButton,
  SidebarProvider,
  useSidebar,
} from '@/components/ui/sidebar';

// Windows 拖拽区域高度（更大，方便用户拖动窗口）
const isWin = isWindows();
const DRAG_HEIGHT = isWin ? 'h-10' : 'h-7';
// Sidebar header 的 padding-top 也需要相应调整
const HEADER_PT = isWin ? 'pt-[calc(40px+1.2rem)]' : 'pt-[calc(28px+1.2rem)]';

/** 窗口控制按钮（红绿灯风格） */
function WindowControls() {
  const tauriWindow = getCurrentWindow();
  const t = useT();

  if (!tauriWindow) return null;

  return (
    <div className="flex items-center gap-2 z-[51]">
      {/* 关闭按钮（红色） */}
      <button
        type="button"
        onClick={() => tauriWindow.close()}
        className="group w-3 h-3 rounded-full bg-red-500/80 hover:bg-red-600 flex items-center justify-center transition-colors"
        title={t('common.close')}
      >
        <X className="w-2.5 h-2.5 text-white/0 group-hover:text-white transition-opacity" />
      </button>

      {/* 最小化按钮（黄色） */}
      <button
        type="button"
        onClick={() => tauriWindow.minimize()}
        className="group w-3 h-3 rounded-full bg-yellow-500/80 hover:bg-yellow-600 flex items-center justify-center transition-colors"
        title={t('common.minimize')}
      >
        <Minus className="w-2.5 h-2.5 text-white/0 group-hover:text-white transition-opacity" />
      </button>

      {/* 最大化/还原按钮（绿色） */}
      <button
        type="button"
        onClick={() => {
          tauriWindow.isMaximized().then((maximized) => {
            if (maximized) {
              tauriWindow.unmaximize();
            } else {
              tauriWindow.maximize();
            }
          });
        }}
        className="group w-3 h-3 rounded-full bg-green-500/80 hover:bg-green-600 flex items-center justify-center transition-colors"
        title={t('common.maximize')}
      >
        <Square className="w-2 h-2 text-white/0 group-hover:text-white transition-opacity" />
      </button>
    </div>
  );
}

const navItems = [
  { path: '/fm', labelKey: 'nav.fm', icon: Radio },
  { path: '/providers', labelKey: 'nav.providers', icon: Cloud },
  { path: '/local', labelKey: 'nav.local', icon: Server },
  { path: '/combos', labelKey: 'nav.combos', icon: Combine },
  { path: '/keys', labelKey: 'nav.keys', icon: Key },
  { path: '/stats', labelKey: 'nav.stats', icon: BarChart3 },
  { path: '/settings', labelKey: 'nav.settings', icon: Settings },
];

function CollapseButton() {
  const t = useT();
  const { toggleSidebar } = useSidebar();

  return (
    <SidebarMenuButton asChild tooltip={t('nav.collapse')}>
      <button type="button" className="w-full" onClick={toggleSidebar}>
        <PanelLeft />
        <span>{t('nav.collapse')}</span>
      </button>
    </SidebarMenuButton>
  );
}

export function AppShell() {
  const t = useT();
  const location = useLocation();
  const isInstall = location.pathname === '/install';

  if (isInstall) {
    return <Outlet />;
  }

  return (
    <>
      <ConnectionStatus />
      <div className="relative">
        <SidebarProvider>
          <Sidebar collapsible="icon">
          {/* 布局遵循 macOS 惯例：顶部透明标题栏带
              → 2rem 间距 → 2.5rem 标题 → 2rem 间距，内容整体为标题栏让位
              Windows 使用更大拖拽区域（40px），方便窗口拖动 */}
          <SidebarHeader className={`shrink-0 items-start border-b border-sidebar-border px-3 pb-[1.2rem] ${HEADER_PT}`}>
            <span className="block truncate pl-[0.2rem] text-[1.2rem] leading-none font-semibold select-none group-data-[collapsible=icon]:invisible">
              XRL Router
            </span>
          </SidebarHeader>
          <SidebarContent>
            <SidebarGroup>
              <SidebarMenu>
                {navItems.map((item) => {
                  const Icon = item.icon;
                  const isActive = location.pathname.startsWith(item.path);
                  return (
                    <SidebarMenuButton
                      key={item.path}
                      asChild
                      isActive={isActive}
                      tooltip={t(item.labelKey)}
                    >
                      <NavLink to={item.path}>
                        <Icon />
                        <span>{t(item.labelKey)}</span>
                      </NavLink>
                    </SidebarMenuButton>
                  );
                })}
              </SidebarMenu>
            </SidebarGroup>
          </SidebarContent>
          <SidebarFooter>
            <SidebarMenu>
              <CollapseButton />
            </SidebarMenu>
          </SidebarFooter>
        </Sidebar>

        <SidebarInset>
          <main className="flex-1 min-w-0 p-8 flex flex-col">
            <Outlet />
          </main>
          {/* 系统资源状态栏：仅在私有智能页面显示 */}
          {location.pathname === '/local' && <SystemStatusBar />}
        </SidebarInset>
        </SidebarProvider>
        {/* 窗口顶部透明拖拽横条，贯穿全宽。
            macOS: 28px（匹配红绿灯按钮区域）
            Windows: 40px（更大拖拽区域，便于窗口拖动） */}
        <div
          data-tauri-drag-region
          className={`fixed inset-x-0 top-0 z-50 ${DRAG_HEIGHT}`}
        />
        {/* 红绿灯窗口控制按钮（仅 Windows 显示，macOS 使用原生） */}
        {isWin && (
          <div className="fixed top-4 left-4 z-[51]">
            <WindowControls />
          </div>
        )}
      </div>
      <PluginRegisterDialog />
    </>
  );
}
