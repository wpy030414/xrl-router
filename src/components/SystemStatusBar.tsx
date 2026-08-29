import { useState, useEffect } from 'react';
import { Cpu, MemoryStick } from 'lucide-react';
import { invoke } from '@tauri-apps/api/core';
import { useT } from '@/i18n';

interface SystemResources {
  cpu_usage: number;
  used_memory: number;
  total_memory: number;
}

function formatBytes(bytes: number): string {
  if (bytes === 0) return '0 B';
  const k = 1024;
  const sizes = ['B', 'KB', 'MB', 'GB', 'TB'];
  const i = Math.floor(Math.log(bytes) / Math.log(k));
  return `${(bytes / Math.pow(k, i)).toFixed(1)} ${sizes[i]}`;
}

export function SystemStatusBar() {
  const t = useT();
  const [resources, setResources] = useState<SystemResources | null>(null);

  useEffect(() => {
    const fetchResources = async () => {
      try {
        const res = await invoke<SystemResources>('get_system_resources');
        setResources(res);
      } catch (err) {
        console.error('Failed to fetch system resources:', err);
      }
    };

    // 立即获取一次
    fetchResources();

    // 每 2 秒刷新一次
    const interval = setInterval(fetchResources, 2000);
    return () => clearInterval(interval);
  }, []);

  if (!resources) {
    return null;
  }

  const memoryPercent = (resources.used_memory / resources.total_memory) * 100;

  return (
    <div className="flex items-center gap-4 px-4 py-2 bg-muted/50 border-t text-xs text-muted-foreground">
      <div className="ml-auto flex items-center gap-4">
        {/* CPU 使用率 */}
        <div className="flex items-center gap-2">
          <Cpu className="w-3.5 h-3.5" />
          <span className="font-medium">{t('system.cpu')}:</span>
          <span className={resources.cpu_usage > 80 ? 'text-red-500' : ''}>
            {resources.cpu_usage.toFixed(1)}%
          </span>
        </div>

        {/* 内存使用 */}
        <div className="flex items-center gap-2">
          <MemoryStick className="w-3.5 h-3.5" />
          <span className="font-medium">{t('system.memory')}:</span>
          <span className={memoryPercent > 80 ? 'text-red-500' : ''}>
            {formatBytes(resources.used_memory)} / {formatBytes(resources.total_memory)}
          </span>
          <span className="text-muted-foreground/60">({memoryPercent.toFixed(1)}%)</span>
        </div>
      </div>
    </div>
  );
}
