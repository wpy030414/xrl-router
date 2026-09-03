import { useState, useEffect, useMemo, useCallback, useRef } from 'react';
import {
  AreaChart,
  Area,
  XAxis,
  YAxis,
  CartesianGrid,
  Tooltip,
  ResponsiveContainer,
} from 'recharts';
import { ChevronLeft, ChevronRight, Inbox } from 'lucide-react';
import { Button } from '@/components/ui/button';
import { Badge } from '@/components/ui/badge';
import {
  Table, TableBody, TableCell, TableHead, TableHeader, TableRow,
} from '@/components/ui/table';
import {
  Select, SelectContent, SelectItem, SelectTrigger, SelectValue,
} from '@/components/ui/select';
import {
  statsApi,
  requestLogApi,
  serviceKeysApi,
  type StatsRow,
  type TopModel,
  type RequestLogRow,
  type ServiceKey,
} from '@/lib/api';
import { useWebSocket } from '@/hooks/useWebSocket';
import { useT } from '@/i18n';
import { cn } from '@/lib/utils';

type TimeRange = 'today' | '1d' | '7d' | '30d';

const TIME_RANGES: { key: TimeRange; labelKey: string }[] = [
  { key: 'today', labelKey: 'stats.range.today' },
  { key: '1d', labelKey: 'stats.range.1d' },
  { key: '7d', labelKey: 'stats.range.7d' },
  { key: '30d', labelKey: 'stats.range.30d' },
];

/** 全数：千分位原样展示（总消耗磁贴主值）。 */
function formatTokens(num: number): string {
  return Math.round(num).toLocaleString();
}

/** 简略值：亿 / 万 结尾，保留 2 位小数（与 Vue 版一致）。 */
function formatTokensAuto(num: number, t: (key: string) => string): string {
  const yi = num / 1e8;
  if (yi >= 1) return `${yi.toFixed(2)}${t('stats.unit_yi')}`;
  const wan = num / 1e4;
  return `${wan.toFixed(2)}${t('stats.unit_wan')}`;
}

/** 后端 bucket 标签（h{epochHours} / d{epochDays}）→ 本地时间短标签，供 X 轴。 */
function formatBucket(label: string): string {
  const m = label.match(/^([hd])(\d+)$/);
  if (!m) return label;
  const step = m[1] === 'h' ? 3600 : 86400;
  // getTimezoneOffset() 返回分钟（UTC+8 = -480），转为秒并取负（与 Vue 版一致）
  const tzOffsetSec = -getTzOffset() * 60;
  const secs = parseInt(m[2], 10) * step - tzOffsetSec;
  const d = new Date(secs * 1000);
  if (m[1] === 'h') return `${String(d.getHours()).padStart(2, '0')}:00`;
  return `${d.getMonth() + 1}/${d.getDate()}`;
}

/** 长标签（tooltip）：小时桶 → M/D HH:00，天桶 → M/D。 */
function formatBucketLong(label: string): string {
  const m = label.match(/^([hd])(\d+)$/);
  if (!m) return label;
  const step = m[1] === 'h' ? 3600 : 86400;
  // getTimezoneOffset() 返回分钟（UTC+8 = -480），转为秒并取负（与 Vue 版一致）
  const tzOffsetSec = -getTzOffset() * 60;
  const secs = parseInt(m[2], 10) * step - tzOffsetSec;
  const d = new Date(secs * 1000);
  if (m[1] === 'h') return `${d.getMonth() + 1}/${d.getDate()} ${String(d.getHours()).padStart(2, '0')}:00`;
  return `${d.getMonth() + 1}/${d.getDate()}`;
}

/** Compute time range boundaries */
function getTimeBounds(range: TimeRange): { from: number; to: number; granularity: 'hour' | 'day' } {
  const now = new Date();
  let from: Date;
  let granularity: 'hour' | 'day';

  switch (range) {
    case 'today':
      from = new Date(now.getFullYear(), now.getMonth(), now.getDate());
      granularity = 'hour';
      break;
    case '1d':
      from = new Date(now.getTime() - 24 * 3600 * 1000);
      granularity = 'hour';
      break;
    case '7d':
      from = new Date(now.getTime() - 7 * 24 * 3600 * 1000);
      granularity = 'day';
      break;
    case '30d':
      from = new Date(now.getTime() - 30 * 24 * 3600 * 1000);
      granularity = 'day';
      break;
  }

  return {
    from: Math.floor(from.getTime() / 1000),
    to: Math.floor(now.getTime() / 1000),
    granularity,
  };
}

/** Get timezone offset in minutes */
function getTzOffset(): number {
  return new Date().getTimezoneOffset();
}

// ── Animated Stats Hook ──
// 数字翻动动画：与 Vue 版 useAnimatedNumber 同策略
// - requestAnimationFrame + easeOutSine 缓动，2400ms 内从旧值平滑翻动到新值
// - 缓存命中率基于动画中的 input/output/cache 值实时计算，而非直接用聚合值
interface AnimatedStats {
  totalTokens: number;
  totalRequests: number;
  totalInput: number;
  totalOutput: number;
  totalCache: number;
  cacheHitRate: string;
}

function useAnimatedStats(aggregated: {
  totalTokens: number;
  totalRequests: number;
  totalInput: number;
  totalOutput: number;
  totalCache: number;
}): AnimatedStats {
  const DURATION = 2400; // ms，与 Vue 版一致

  const totalTokensRef = useRef(0);
  const totalRequestsRef = useRef(0);
  const totalInputRef = useRef(0);
  const totalOutputRef = useRef(0);
  const totalCacheRef = useRef(0);

  const [display, setDisplay] = useState<AnimatedStats>({
    totalTokens: 0,
    totalRequests: 0,
    totalInput: 0,
    totalOutput: 0,
    totalCache: 0,
    cacheHitRate: '0.0',
  });

  const animationIdRef = useRef<number | null>(null);

  useEffect(() => {
    if (animationIdRef.current) {
      cancelAnimationFrame(animationIdRef.current);
    }

    const startTotalTokens = totalTokensRef.current;
    const startTotalRequests = totalRequestsRef.current;
    const startTotalInput = totalInputRef.current;
    const startTotalOutput = totalOutputRef.current;
    const startTotalCache = totalCacheRef.current;
    const startTime = performance.now();

    function tick(now: number) {
      const elapsed = now - startTime;
      const progress = Math.min(elapsed / DURATION, 1);
      // easeOutSine: 前 1/3 快速翻动，后 2/3 缓慢趋近目标值
      const ease = Math.sin(progress * Math.PI / 2);

      const curTotalTokens = Math.round(startTotalTokens + (aggregated.totalTokens - startTotalTokens) * ease);
      const curTotalRequests = Math.round(startTotalRequests + (aggregated.totalRequests - startTotalRequests) * ease);
      const curTotalInput = Math.round(startTotalInput + (aggregated.totalInput - startTotalInput) * ease);
      const curTotalOutput = Math.round(startTotalOutput + (aggregated.totalOutput - startTotalOutput) * ease);
      const curTotalCache = Math.round(startTotalCache + (aggregated.totalCache - startTotalCache) * ease);

      // 缓存命中率基于动画中的值实时计算（与 Vue 版一致）
      const base = curTotalInput + curTotalCache;
      const cacheHitRate = base > 0 ? ((curTotalCache / base) * 100).toFixed(1) : '0.0';

      totalTokensRef.current = curTotalTokens;
      totalRequestsRef.current = curTotalRequests;
      totalInputRef.current = curTotalInput;
      totalOutputRef.current = curTotalOutput;
      totalCacheRef.current = curTotalCache;

      setDisplay({
        totalTokens: curTotalTokens,
        totalRequests: curTotalRequests,
        totalInput: curTotalInput,
        totalOutput: curTotalOutput,
        totalCache: curTotalCache,
        cacheHitRate,
      });

      if (progress < 1) {
        animationIdRef.current = requestAnimationFrame(tick);
      }
    }

    animationIdRef.current = requestAnimationFrame(tick);

    return () => {
      if (animationIdRef.current) {
        cancelAnimationFrame(animationIdRef.current);
      }
    };
  }, [aggregated]);

  return display;
}

// ── Stat Tile Component ──
interface StatTileProps {
  label: string;
  value: string;
  sub?: string;
  className?: string;
}

function StatTile({ label, value, sub, className }: StatTileProps) {
  return (
    <div className={cn('rounded-xl border bg-card p-4 space-y-1', className)}>
      <div className="text-xs text-muted-foreground">{label}</div>
      <div className="text-xl font-semibold tabular-nums">
        {value}
        {/* 简略值衔在大数字之后（同行、小号、灰色），而非换行下方 */}
        {sub && (
          <span className="ml-1.5 text-xs font-normal text-muted-foreground">{sub}</span>
        )}
      </div>
    </div>
  );
}

// ── Custom Chart Tooltip ──
interface ChartTooltipProps {
  active?: boolean;
  payload?: Array<{ value: number; dataKey: string; color: string }>;
  label?: string;
}

function ChartTooltip({ active, payload, label }: ChartTooltipProps) {
  if (!active || !payload?.length) return null;

  return (
    <div className="rounded-lg border bg-popover p-3 shadow-md text-sm">
      <div className="font-medium mb-1.5">{formatBucketLong(String(label))}</div>
      {payload.map((entry, idx) => (
        <div key={idx} className="flex items-center gap-2 text-xs">
          <span
            className="w-2 h-2 rounded-full"
            style={{ backgroundColor: entry.color }}
          />
          <span className="text-muted-foreground">{entry.dataKey}:</span>
          <span className="font-medium tabular-nums">
            {(entry.value / 1000).toFixed(1)}K
          </span>
        </div>
      ))}
    </div>
  );
}

// ── Main View ──
export function StatsView() {
  const t = useT();
  const [timeRange, setTimeRange] = useState<TimeRange>('today');
  const [loading, setLoading] = useState(true);
  const [rows, setRows] = useState<StatsRow[]>([]);
  const [topModel, setTopModel] = useState<TopModel | null>(null);

  // 密钥筛选：作用于下方全部统计单元（磁贴 / 趋势图 / 请求日志）
  const [serviceKeys, setServiceKeys] = useState<ServiceKey[]>([]);
  const [keyFilter, setKeyFilter] = useState('');

  // Request log state
  const [logRows, setLogRows] = useState<RequestLogRow[]>([]);
  const [logTotal, setLogTotal] = useState(0);
  const [logPage, setLogPage] = useState(1);
  const [logLoading, setLogLoading] = useState(true);
  const LOG_PAGE_SIZE = 15;

  const fetchStats = async (range: TimeRange) => {
    const { from, to, granularity } = getTimeBounds(range);
    try {
      const result = await statsApi.query({
        from,
        to,
        granularity,
        // getTimezoneOffset() 返回分钟（UTC+8 = -480），转为秒并取负（与 Vue 版一致）
        tz_offset: -getTzOffset() * 60,
        service_key_id: keyFilter || undefined,
      });
      setRows(result.data || []);
      setTopModel(result.top_model || null);
    } catch (e: any) {
      console.error('Failed to fetch stats:', e);
    } finally {
      setLoading(false);
    }
  };

  // silent = true：后台定时刷新用，不置 logLoading，避免表格每 5s 闪一次半透明。
  const fetchLog = async (page: number, silent = false) => {
    if (!silent) setLogLoading(true);
    try {
      // 与统计同源的时间范围：日志也跟随「当天/一天内/…」与密钥筛选
      const { from, to } = getTimeBounds(timeRange);
      const result = await requestLogApi.page({
        page,
        page_size: LOG_PAGE_SIZE,
        from,
        to,
        service_key_id: keyFilter || undefined,
      });
      setLogRows(result.data || []);
      setLogTotal(result.total);
    } catch (e: any) {
      console.error('Failed to fetch request log:', e);
    } finally {
      if (!silent) setLogLoading(false);
    }
  };

  // Real-time refresh via WebSocket：统计与请求日志同源时间范围，一起刷新。
  // 后端每 5s 广播一次 usage_stats_changed。
  const refreshTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  useEffect(() => {
    return () => {
      if (refreshTimer.current) clearTimeout(refreshTimer.current);
    };
  }, []);
  useWebSocket('usage_stats_changed', useCallback(() => {
    // Debounce：5s 一次的广播足够密，没必要每次都打接口，1s 内合并。
    if (refreshTimer.current) return;
    refreshTimer.current = setTimeout(() => {
      refreshTimer.current = null;
      fetchStats(timeRange);
      // 只刷第一页：第 N 页在刷新时会把最新记录顶进前面几页，只有第一页
      // 内容稳定变化；老页刷新反而会因插入导致重复/漏行。
      if (logPage === 1) fetchLog(1, true);
    }, 1000);
  }, [timeRange, keyFilter, logPage]));

  // Load stats on range / key change；切换时间范围时日志回到第一页
  useEffect(() => {
    setLoading(true);
    fetchStats(timeRange);
    setLogPage(1);
  }, [timeRange, keyFilter]);

  // Load request log on page / key / range change
  useEffect(() => {
    fetchLog(logPage);
  }, [logPage, keyFilter, timeRange]);

  // 密钥筛选下拉数据
  useEffect(() => {
    serviceKeysApi.list().then(setServiceKeys).catch(() => {});
  }, []);

  // Computed aggregations
  const aggregated = useMemo(() => {
    const totalTokens = rows.reduce((sum, r) => sum + r.total_tokens, 0);
    const totalRequests = rows.reduce((sum, r) => sum + r.requests, 0);
    const totalInput = rows.reduce((sum, r) => sum + r.prompt_tokens, 0);
    const totalOutput = rows.reduce((sum, r) => sum + r.completion_tokens, 0);
    const totalCache = rows.reduce((sum, r) => sum + r.cache_read_input_tokens, 0);
    // 缓存命中率 = 命中 Tokens / (未命中输入 + 命中 Tokens)。
    // 后端把 prompt_tokens 拆为互斥的「未命中输入」与 cache_read_input_tokens，
    // 分母必须包含命中部分，否则会超过 100%（与 Vue 版算法一致）。
    const base = totalInput + totalCache;
    const cacheHitRate = base > 0 ? (totalCache / base) * 100 : 0;

    return { totalTokens, totalRequests, totalInput, totalOutput, totalCache, cacheHitRate };
  }, [rows]);

  // 数字翻动动画：与 Vue 版同策略，平滑过渡到新值
  const anim = useAnimatedStats(aggregated);

  // Chart data：生成完整的时间序列，没有数据的桶填0
  const chartData = useMemo(() => {
    const { from, to, granularity } = getTimeBounds(timeRange);
    const step = granularity === 'hour' ? 3600 : 86400;
    const prefix = granularity === 'hour' ? 'h' : 'd';
    // 后端 bucket = floor((utc_timestamp + tz_offset) / step)，前端也要加 tz_offset 才能匹配
    const tzOffsetSec = -getTzOffset() * 60; // getTimezoneOffset() 返回分钟（UTC+8 = -480），转为秒并取负

    // 生成完整的时间桶序列
    const start = Math.floor((from + tzOffsetSec) / step);
    const end = Math.floor((to + tzOffsetSec) / step);
    const buckets: string[] = [];
    for (let b = start; b <= end; b++) {
      buckets.push(`${prefix}${b}`);
    }

    // 把后端数据按 bucket 分组
    const dataMap = new Map<string, { input: number; output: number; cache: number }>();
    for (const r of rows) {
      const existing = dataMap.get(r.day) || { input: 0, output: 0, cache: 0 };
      existing.input += r.prompt_tokens;
      existing.output += r.completion_tokens;
      existing.cache += r.cache_read_input_tokens;
      dataMap.set(r.day, existing);
    }

    // 生成完整序列，没有数据的桶填0
    return buckets.map((bucket) => {
      const data = dataMap.get(bucket) || { input: 0, output: 0, cache: 0 };
      return {
        time: bucket,
        input: data.input,
        output: data.output,
        cache: data.cache,
      };
    });
  }, [rows, timeRange]);

  const logTotalPages = Math.max(1, Math.ceil(logTotal / LOG_PAGE_SIZE));

  // 键盘左右键翻页请求日志（仅在非输入焦点时生效）
  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      // 输入框 / 下拉框聚焦时不拦截，避免影响正常输入
      const target = e.target as HTMLElement;
      if (target.tagName === 'INPUT' || target.tagName === 'TEXTAREA' || target.tagName === 'SELECT' || target.isContentEditable) {
        return;
      }
      if (e.key === 'ArrowLeft') {
        setLogPage((p) => Math.max(1, p - 1));
      } else if (e.key === 'ArrowRight') {
        setLogPage((p) => Math.min(logTotalPages, p + 1));
      }
    };
    window.addEventListener('keydown', handleKeyDown);
    return () => window.removeEventListener('keydown', handleKeyDown);
  }, [logTotalPages]);

  return (
    <div className="space-y-6">
      {/* Header */}
      <div className="flex justify-between items-start gap-4 flex-wrap">
        <h2 className="text-3xl font-normal m-0">{t('stats.title')}</h2>
        <div className="flex items-center gap-2.5">
          {/* 密钥筛选：作用于全部统计单元 */}
          <Select
            value={keyFilter}
            onValueChange={(v) => {
              setKeyFilter(v);
              setLogPage(1);
            }}
          >
            <SelectTrigger className="h-9 w-[200px]">
              <SelectValue placeholder={t('common.all')} />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="">{t('common.all')}</SelectItem>
              {serviceKeys.map((k) => (
                <SelectItem key={k.id} value={k.id}>
                  {k.name} ({k.key_masked})
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
          <div className="flex gap-1 rounded-lg border bg-muted/50 p-1">
            {TIME_RANGES.map((r) => (
              <button
                key={r.key}
                onClick={() => setTimeRange(r.key)}
                className={cn(
                  'px-3 py-1.5 rounded-md text-sm font-medium transition-colors',
                  timeRange === r.key
                    ? 'bg-background shadow-sm text-foreground'
                    : 'text-muted-foreground hover:text-foreground'
                )}
              >
                {t(r.labelKey)}
              </button>
            ))}
          </div>
        </div>
      </div>

      {/* Stat Tiles：第一行 总消耗(2列宽) + 调用最多模型 + 总请求次；第二行 输入/输出/命中/命中率 */}
      <div className="grid grid-cols-2 sm:grid-cols-4 gap-3">
        <StatTile
          label={t('stats.tile.total_tokens')}
          value={formatTokens(anim.totalTokens)}
          sub={`≈${formatTokensAuto(anim.totalTokens, t)}`}
          className="sm:col-span-2"
        />
        <StatTile
          label={t('stats.tile.top_model')}
          value={topModel?.model_name || '—'}
        />
        <StatTile
          label={t('stats.tile.total_requests')}
          value={formatTokens(anim.totalRequests)}
        />
        <StatTile
          label={t('stats.tile.input_tokens')}
          value={formatTokensAuto(anim.totalInput, t)}
        />
        <StatTile
          label={t('stats.tile.output_tokens')}
          value={formatTokensAuto(anim.totalOutput, t)}
        />
        <StatTile
          label={t('stats.tile.cache_tokens')}
          value={formatTokensAuto(anim.totalCache, t)}
        />
        <StatTile
          label={t('stats.tile.cache_hit_rate')}
          value={`${anim.cacheHitRate}%`}
        />
      </div>

      {/* Usage Trend Chart */}
      <div className="rounded-xl border bg-card p-5 space-y-3">
        <h3 className="text-sm font-medium">{t('stats.chart.title')}</h3>
        {loading ? (
          <div className="flex items-center justify-center h-[300px]">
            <div className="animate-spin rounded-full h-6 w-6 border-b-2 border-primary" />
          </div>
        ) : chartData.length === 0 ? (
          <div className="flex flex-col items-center justify-center h-[300px] text-muted-foreground">
            <Inbox className="w-8 h-8 mb-2" />
            <span className="text-sm">{t('common.empty')}</span>
          </div>
        ) : (
          <ResponsiveContainer width="100%" height={300}>
            <AreaChart data={chartData} margin={{ top: 10, right: 10, left: 0, bottom: 0 }}>
              <defs>
                <linearGradient id="colorInput" x1="0" y1="0" x2="0" y2="1">
                  <stop offset="5%" stopColor="hsl(217, 91%, 60%)" stopOpacity={0.3} />
                  <stop offset="95%" stopColor="hsl(217, 91%, 60%)" stopOpacity={0} />
                </linearGradient>
                <linearGradient id="colorOutput" x1="0" y1="0" x2="0" y2="1">
                  <stop offset="5%" stopColor="hsl(142, 71%, 45%)" stopOpacity={0.3} />
                  <stop offset="95%" stopColor="hsl(142, 71%, 45%)" stopOpacity={0} />
                </linearGradient>
                <linearGradient id="colorCache" x1="0" y1="0" x2="0" y2="1">
                  <stop offset="5%" stopColor="hsl(45, 93%, 47%)" stopOpacity={0.3} />
                  <stop offset="95%" stopColor="hsl(45, 93%, 47%)" stopOpacity={0} />
                </linearGradient>
              </defs>
              <CartesianGrid strokeDasharray="3 3" className="stroke-muted" />
              <XAxis
                dataKey="time"
                tick={{ fontSize: 11 }}
                tickLine={false}
                axisLine={false}
                className="text-muted-foreground"
                // 与 Vue 版同策略：bucket 转为本地时间短标签；点数超过容量时每 N 个显示一个
                tickFormatter={(v: string) => formatBucket(v)}
                interval={Math.max(0, Math.ceil(chartData.length / 7) - 1)}
              />
              <YAxis
                tick={{ fontSize: 11 }}
                tickLine={false}
                axisLine={false}
                tickFormatter={(v) => `${(Number(v) / 10000).toFixed(0)}`}
                className="text-muted-foreground"
              />
              <Tooltip content={<ChartTooltip />} />
              <Area
                type="monotone"
                dataKey="cache"
                stroke="hsl(45, 93%, 47%)"
                fill="url(#colorCache)"
                strokeWidth={2}
                name="Cache"
              />
              <Area
                type="monotone"
                dataKey="output"
                stroke="hsl(142, 71%, 45%)"
                fill="url(#colorOutput)"
                strokeWidth={2}
                name="Output"
              />
              <Area
                type="monotone"
                dataKey="input"
                stroke="hsl(217, 91%, 60%)"
                fill="url(#colorInput)"
                strokeWidth={2}
                name="Input"
              />
            </AreaChart>
          </ResponsiveContainer>
        )}
      </div>

      {/* Request Log */}
      <div className="rounded-xl border bg-card p-5 space-y-4">
        <div className="flex justify-between items-center">
          <h3 className="text-sm font-medium">
            {t('stats.log.title')}
            <span className="text-muted-foreground font-normal ml-2 text-xs">
              {t('stats.log.total', { total: logTotal })}
            </span>
          </h3>
        </div>

        {logRows.length === 0 ? (
          logLoading ? (
            <div className="flex items-center justify-center py-8">
              <div className="animate-spin rounded-full h-6 w-6 border-b-2 border-primary" />
            </div>
          ) : (
            <div className="flex flex-col items-center justify-center py-8 text-muted-foreground">
              <Inbox className="w-8 h-8 mb-2" />
              <span className="text-sm">{t('stats.log.empty')}</span>
            </div>
          )
        ) : (
          <>
            <div className={cn('transition-opacity', logLoading && 'opacity-50')}>
              <Table>
                <TableHeader>
                  <TableRow>
                    <TableHead>{t('stats.log.col_time')}</TableHead>
                    <TableHead>{t('stats.log.col_key')}</TableHead>
                    <TableHead>{t('stats.log.col_provider')}</TableHead>
                    <TableHead>{t('stats.log.col_model')}</TableHead>
                    <TableHead className="text-right">{t('stats.log.col_input')}</TableHead>
                    <TableHead className="text-right">{t('stats.log.col_output')}</TableHead>
                    <TableHead className="text-center">{t('stats.log.col_status')}</TableHead>
                  </TableRow>
                </TableHeader>
                <TableBody>
                  {logRows.map((row) => (
                    <TableRow key={row.id}>
                      <TableCell className="text-xs tabular-nums">
                        {new Date(row.timestamp * 1000).toLocaleString()}
                      </TableCell>
                      <TableCell className="text-xs truncate max-w-[120px]" title={row.service_key_name}>
                        {row.service_key_name || row.service_key_masked}
                      </TableCell>
                      <TableCell className="text-xs">{row.provider_name}</TableCell>
                      <TableCell className="text-xs truncate max-w-[150px]" title={row.model_display_name}>
                        {row.model_display_name}
                      </TableCell>
                      <TableCell className="text-xs text-right tabular-nums">
                        {row.prompt_tokens.toLocaleString()}
                      </TableCell>
                      <TableCell className="text-xs text-right tabular-nums">
                        {row.completion_tokens.toLocaleString()}
                      </TableCell>
                      <TableCell className="text-center">
                        <Badge
                          variant={row.success ? 'secondary' : 'destructive'}
                          className="text-xs"
                          title={row.error_message || undefined}
                        >
                          {row.success ? t('stats.log.status_ok') : t('stats.log.status_fail')}
                        </Badge>
                      </TableCell>
                    </TableRow>
                  ))}
                </TableBody>
              </Table>
            </div>

            {/* Pagination */}
            <div className="flex items-center justify-between pt-2">
              <Button
                variant="outline"
                size="sm"
                disabled={logPage <= 1}
                onClick={() => setLogPage((p) => p - 1)}
              >
                <ChevronLeft className="w-4 h-4 mr-1" />
                {t('stats.log.prev')}
              </Button>
              <span className="text-sm text-muted-foreground">
                {t('stats.log.page', { current: logPage, total: logTotalPages })}
              </span>
              <Button
                variant="outline"
                size="sm"
                disabled={logPage >= logTotalPages}
                onClick={() => setLogPage((p) => p + 1)}
              >
                {t('stats.log.next')}
                <ChevronRight className="w-4 h-4 ml-1" />
              </Button>
            </div>
          </>
        )}
      </div>
    </div>
  );
}

export default StatsView;
