import { Outlet, NavLink, useLocation } from 'react-router-dom';
import { Radio, Cloud, GitMerge, Key, BarChart3, Settings, PanelLeft } from 'lucide-react';
import { useT } from '@/i18n';
import { ConnectionStatus } from './ConnectionStatus';
import { PluginRegisterDialog } from './PluginRegisterDialog';
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

const navItems = [
  { path: '/fm', labelKey: 'nav.fm', icon: Radio },
  { path: '/providers', labelKey: 'nav.providers', icon: Cloud },
  { path: '/combos', labelKey: 'nav.combos', icon: GitMerge },
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
          {/* 布局遵循 macOS 惯例：顶部 28px 透明标题栏带（红绿灯悬浮）
              → 2rem 间距 → 2.5rem 标题 → 2rem 间距，内容整体为标题栏让位 */}
          <SidebarHeader className="shrink-0 items-start border-b border-sidebar-border px-3 pt-[calc(28px+1.2rem)] pb-[1.2rem]">
            <span className="block truncate pl-[0.2rem] text-[1.2rem] leading-none font-semibold select-none group-data-[collapsible=icon]:hidden">
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
          <main className="flex-1 min-w-0 p-8 grid grid-cols-[1fr_minmax(0,880px)_1fr]">
            <div className="col-start-2">
              <Outlet />
            </div>
          </main>
        </SidebarInset>
        </SidebarProvider>
        {/* 窗口顶部 28px 透明拖拽横条（= macOS 标题栏带），贯穿全宽。
            仅此一条 drag region：侧边栏 header、main 内容及各处空白均不参与拖动 */}
        <div
          data-tauri-drag-region
          className="absolute inset-x-0 top-0 z-50 h-7"
        />
      </div>
      <PluginRegisterDialog />
    </>
  );
}
