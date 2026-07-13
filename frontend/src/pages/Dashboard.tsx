import { useMemo } from "react";
import { useQuery } from "@tanstack/react-query";
import { AppLayout } from "@/components/layout/AppLayout";
import { StatCard } from "@/components/dashboard/StatCard";
import { Activity, GitBranch, Users, DollarSign, Clock, Sparkles } from "lucide-react";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import {
  AreaChart,
  Area,
  XAxis,
  YAxis,
  CartesianGrid,
  Tooltip,
  ResponsiveContainer,
} from "recharts";
import {
  fetchMetricsDaily,
  fetchMetricsOverview,
  fetchTraces,
} from "@/lib/xtrace";

const formatCount = (n: number) => n.toLocaleString();

const formatCost = (n: number) => {
  if (!Number.isFinite(n) || n === 0) return "$0.00";
  if (n < 0.01) return `$${n.toFixed(6)}`;
  return `$${n.toFixed(2)}`;
};

const formatLatencyMs = (ms: number) => {
  if (!Number.isFinite(ms) || ms <= 0) return "0ms";
  if (ms >= 1000) return `${(ms / 1000).toFixed(2)}s`;
  return `${ms.toFixed(0)}ms`;
};

export default function Dashboard() {
  const overviewQuery = useQuery({
    queryKey: ["metrics-overview", "traces"],
    queryFn: () => fetchMetricsOverview("traces"),
  });

  const tracesQuery = useQuery({
    queryKey: ["traces", "dashboard"],
    queryFn: () => fetchTraces(100),
  });

  const dailyQuery = useQuery({
    queryKey: ["metrics-daily"],
    queryFn: () => fetchMetricsDaily(14),
  });

  const overview = overviewQuery.data?.data?.[0];
  const traces = useMemo(
    () => tracesQuery.data?.data ?? [],
    [tracesQuery.data?.data],
  );

  const stats = useMemo(() => {
    const totalTraces = overview?.count_count ?? tracesQuery.data?.meta.totalItems ?? 0;
    const sessions = new Set(traces.map((t) => t.sessionId).filter(Boolean)).size;
    const users = new Set(traces.map((t) => t.userId).filter(Boolean)).size;
    const totalCost = traces.reduce((sum, t) => sum + (t.totalCost ?? 0), 0);
    const avgLatencyMs = overview?.avg_latency ?? 0;
    const errorCount = overview?.error_count ?? 0;
    const successRate =
      totalTraces > 0
        ? Math.max(0, Math.min(100, ((totalTraces - errorCount) / totalTraces) * 100))
        : 100;
    return { totalTraces, sessions, users, totalCost, avgLatencyMs, errorCount, successRate };
  }, [overview, traces, tracesQuery.data?.meta.totalItems]);

  const chartData = useMemo(() => {
    const daily = dailyQuery.data?.data ?? [];
    if (daily.length > 0) {
      return [...daily]
        .reverse()
        .map((d) => ({
          name: d.date?.slice(5) || d.date || "-",
          traces: Number(d.countTraces ?? 0),
          cost: Number(d.totalCost ?? 0),
        }));
    }
    // Fallback: bucket recent traces by UTC hour
    const buckets = new Map<string, { traces: number; cost: number }>();
    for (const t of traces) {
      const ts = t.timestamp || t.createdAt;
      if (!ts) continue;
      const d = new Date(ts);
      if (Number.isNaN(d.getTime())) continue;
      const key = `${String(d.getUTCHours()).padStart(2, "0")}:00`;
      const cur = buckets.get(key) ?? { traces: 0, cost: 0 };
      cur.traces += 1;
      cur.cost += t.totalCost ?? 0;
      buckets.set(key, cur);
    }
    return [...buckets.entries()]
      .sort(([a], [b]) => a.localeCompare(b))
      .map(([name, v]) => ({ name, traces: v.traces, cost: v.cost }));
  }, [dailyQuery.data, traces]);

  const isLoading = overviewQuery.isLoading || tracesQuery.isLoading;
  const isError = overviewQuery.isError || tracesQuery.isError;

  return (
    <AppLayout>
      <div className="space-y-6 animate-fade-in">
        <div>
          <h1 className="text-2xl font-bold text-foreground">Dashboard</h1>
          <p className="text-muted-foreground mt-1">
            Live metrics from your xtrace API (no mock data)
          </p>
        </div>

        {isError && (
          <div className="rounded-lg border border-border bg-card p-4 text-sm text-muted-foreground">
            Failed to load dashboard metrics. Check{" "}
            <code className="text-xs">VITE_XTRACE_BASE_URL</code> / API token.
          </div>
        )}

        {!isLoading && !isError && stats.totalTraces === 0 && (
          <div className="rounded-lg border border-border bg-card p-4 text-sm text-muted-foreground">
            No traces yet. Ingest data via batch/OTLP, then refresh this page.
          </div>
        )}

        <div className="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-4 gap-4">
          <StatCard
            title="Total Traces"
            value={isLoading ? "…" : formatCount(stats.totalTraces)}
            change={isLoading ? undefined : "from /api/public/metrics"}
            changeType="neutral"
            icon={Activity}
          />
          <StatCard
            title="Active Sessions"
            value={isLoading ? "…" : formatCount(stats.sessions)}
            change={isLoading ? undefined : "distinct sessionId (sample)"}
            changeType="neutral"
            icon={GitBranch}
          />
          <StatCard
            title="Users"
            value={isLoading ? "…" : formatCount(stats.users)}
            change={isLoading ? undefined : "distinct userId (sample)"}
            changeType="neutral"
            icon={Users}
          />
          <StatCard
            title="Total Cost"
            value={isLoading ? "…" : formatCost(stats.totalCost)}
            change={isLoading ? undefined : "sum of totalCost (sample)"}
            changeType="neutral"
            icon={DollarSign}
          />
        </div>

        <div className="grid grid-cols-1 lg:grid-cols-2 gap-6">
          <Card>
            <CardHeader className="pb-2">
              <CardTitle className="text-base font-medium">Traces Trend</CardTitle>
            </CardHeader>
            <CardContent>
              <div className="h-[280px]">
                {chartData.length === 0 ? (
                  <div className="h-full flex items-center justify-center text-sm text-muted-foreground">
                    No trend data
                  </div>
                ) : (
                  <ResponsiveContainer width="100%" height="100%">
                    <AreaChart data={chartData}>
                      <defs>
                        <linearGradient id="colorTraces" x1="0" y1="0" x2="0" y2="1">
                          <stop offset="5%" stopColor="hsl(266, 92%, 50%)" stopOpacity={0.3} />
                          <stop offset="95%" stopColor="hsl(266, 92%, 50%)" stopOpacity={0} />
                        </linearGradient>
                      </defs>
                      <CartesianGrid strokeDasharray="3 3" stroke="hsl(var(--border))" />
                      <XAxis
                        dataKey="name"
                        stroke="hsl(var(--muted-foreground))"
                        fontSize={12}
                        tickLine={false}
                        axisLine={false}
                      />
                      <YAxis
                        stroke="hsl(var(--muted-foreground))"
                        fontSize={12}
                        tickLine={false}
                        axisLine={false}
                        allowDecimals={false}
                      />
                      <Tooltip
                        contentStyle={{
                          backgroundColor: "hsl(var(--card))",
                          border: "1px solid hsl(var(--border))",
                          borderRadius: "8px",
                        }}
                      />
                      <Area
                        type="monotone"
                        dataKey="traces"
                        stroke="hsl(266, 92%, 50%)"
                        strokeWidth={2}
                        fillOpacity={1}
                        fill="url(#colorTraces)"
                      />
                    </AreaChart>
                  </ResponsiveContainer>
                )}
              </div>
            </CardContent>
          </Card>

          <Card>
            <CardHeader className="pb-2">
              <CardTitle className="text-base font-medium">Cost Trend</CardTitle>
            </CardHeader>
            <CardContent>
              <div className="h-[280px]">
                {chartData.length === 0 ? (
                  <div className="h-full flex items-center justify-center text-sm text-muted-foreground">
                    No trend data
                  </div>
                ) : (
                  <ResponsiveContainer width="100%" height="100%">
                    <AreaChart data={chartData}>
                      <defs>
                        <linearGradient id="colorCost" x1="0" y1="0" x2="0" y2="1">
                          <stop offset="5%" stopColor="hsl(340, 90%, 55%)" stopOpacity={0.3} />
                          <stop offset="95%" stopColor="hsl(340, 90%, 55%)" stopOpacity={0} />
                        </linearGradient>
                      </defs>
                      <CartesianGrid strokeDasharray="3 3" stroke="hsl(var(--border))" />
                      <XAxis
                        dataKey="name"
                        stroke="hsl(var(--muted-foreground))"
                        fontSize={12}
                        tickLine={false}
                        axisLine={false}
                      />
                      <YAxis
                        stroke="hsl(var(--muted-foreground))"
                        fontSize={12}
                        tickLine={false}
                        axisLine={false}
                        tickFormatter={(value) => `$${value}`}
                      />
                      <Tooltip
                        contentStyle={{
                          backgroundColor: "hsl(var(--card))",
                          border: "1px solid hsl(var(--border))",
                          borderRadius: "8px",
                        }}
                        formatter={(value: number) => [formatCost(Number(value)), "Cost"]}
                      />
                      <Area
                        type="monotone"
                        dataKey="cost"
                        stroke="hsl(340, 90%, 55%)"
                        strokeWidth={2}
                        fillOpacity={1}
                        fill="url(#colorCost)"
                      />
                    </AreaChart>
                  </ResponsiveContainer>
                )}
              </div>
            </CardContent>
          </Card>
        </div>

        <div className="grid grid-cols-1 md:grid-cols-3 gap-4">
          <Card>
            <CardContent className="pt-6">
              <div className="flex items-center gap-4">
                <div className="p-3 rounded-lg bg-xtrace-purple/10">
                  <Sparkles className="h-6 w-6 text-xtrace-purple" />
                </div>
                <div>
                  <p className="text-2xl font-bold">
                    {isLoading ? "…" : formatCount(stats.errorCount)}
                  </p>
                  <p className="text-sm text-muted-foreground">Error traces (overview)</p>
                </div>
              </div>
            </CardContent>
          </Card>

          <Card>
            <CardContent className="pt-6">
              <div className="flex items-center gap-4">
                <div className="p-3 rounded-lg bg-xtrace-orange/10">
                  <Clock className="h-6 w-6 text-xtrace-orange" />
                </div>
                <div>
                  <p className="text-2xl font-bold">
                    {isLoading ? "…" : formatLatencyMs(stats.avgLatencyMs)}
                  </p>
                  <p className="text-sm text-muted-foreground">Avg Latency</p>
                </div>
              </div>
            </CardContent>
          </Card>

          <Card>
            <CardContent className="pt-6">
              <div className="flex items-center gap-4">
                <div className="p-3 rounded-lg bg-xtrace-success/10">
                  <Activity className="h-6 w-6 text-xtrace-success" />
                </div>
                <div>
                  <p className="text-2xl font-bold">
                    {isLoading ? "…" : `${stats.successRate.toFixed(1)}%`}
                  </p>
                  <p className="text-sm text-muted-foreground">Success Rate</p>
                </div>
              </div>
            </CardContent>
          </Card>
        </div>
      </div>
    </AppLayout>
  );
}
