import { lazy, Suspense, type ReactNode } from 'react';
import { createBrowserRouter, Navigate } from 'react-router';
import { AppShell } from './components/AppShell';

const ProvidersView = lazy(() => import('./views/ProvidersView'));
const ProviderFormView = lazy(() => import('./views/ProviderFormView'));
const ServiceKeysView = lazy(() => import('./views/ServiceKeysView'));
const StatsView = lazy(() => import('./views/StatsView'));
const SettingsView = lazy(() => import('./views/SettingsView'));
const InstallView = lazy(() => import('./views/InstallView'));
const FmView = lazy(() => import('./views/FmView'));
const CombosView = lazy(() => import('./views/CombosView'));
const ComboFormView = lazy(() => import('./views/ComboFormView'));
const LocalModelsView = lazy(() => import('./views/LocalModelsView'));
const AuditView = lazy(() => import('./views/AuditView'));

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
      { path: 'local', element: page(<LocalModelsView />) },
      { path: 'combos', element: page(<CombosView />) },
      { path: 'combos/new', element: page(<ComboFormView />) },
      { path: 'combos/:id/edit', element: page(<ComboFormView />) },
      { path: 'keys', element: page(<ServiceKeysView />) },
      { path: 'stats', element: page(<StatsView />) },
      { path: 'audit', element: page(<AuditView />) },
      { path: 'settings', element: page(<SettingsView />) },
      { path: 'fm', element: page(<FmView />) },
    ],
  },
  { path: '/install', element: page(<InstallView />) },
]);
