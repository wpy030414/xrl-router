import { lazy, Suspense, type ReactNode } from 'react';
import { createBrowserRouter, Navigate } from 'react-router';
import { AppShell } from './components/AppShell';

const ProvidersView = lazy(() => import('./views/ProvidersView'));
const ProviderFormView = lazy(() => import('./views/ProviderFormView'));
const KeysView = lazy(() => import('./views/KeysView'));
const StatsView = lazy(() => import('./views/StatsView'));
const SettingsView = lazy(() => import('./views/SettingsView'));
const InstallView = lazy(() => import('./views/InstallView'));
const ClaudeFmView = lazy(() => import('./views/ClaudeFmView'));
const CombosView = lazy(() => import('./views/CombosView'));
const ComboFormView = lazy(() => import('./views/ComboFormView'));

/** 路由级懒加载：本地资源加载极快，无需额外 loading UI */
function page(element: ReactNode) {
  return <Suspense fallback={null}>{element}</Suspense>;
}

export const router = createBrowserRouter([
  {
    path: '/',
    element: <AppShell />,
    children: [
      { index: true, element: <Navigate to="/providers" replace /> },
      { path: 'providers', element: page(<ProvidersView />) },
      { path: 'providers/new', element: page(<ProviderFormView />) },
      { path: 'providers/:id/edit', element: page(<ProviderFormView />) },
      { path: 'combos', element: page(<CombosView />) },
      { path: 'combos/new', element: page(<ComboFormView />) },
      { path: 'combos/:id/edit', element: page(<ComboFormView />) },
      { path: 'keys', element: page(<KeysView />) },
      { path: 'stats', element: page(<StatsView />) },
      { path: 'settings', element: page(<SettingsView />) },
      { path: 'fm', element: page(<ClaudeFmView />) },
    ],
  },
  { path: '/install', element: page(<InstallView />) },
]);
