import { useMemo, useState } from "react";
import { AppLayout } from "@/components/layout/AppLayout";
import { TraceList, TraceListItemView } from "@/components/traces/TraceList";
import { TraceDetail } from "@/components/traces/TraceDetail";
import { useQuery } from "@tanstack/react-query";
import { fetchTrace, fetchTraces, type TraceDetail as TraceDetailType } from "@/lib/xtrace";

const formatSeconds = (value: number | null) => {
  if (value === null || Number.isNaN(value)) return "-";
  return `${value.toFixed(2)}s`;
};

const formatCost = (value: number | null) => {
  if (value === null || Number.isNaN(value)) return "-";
  return `$${value.toFixed(6)}`;
};

const formatTokenText = (value: number | null) => {
  if (value === null || Number.isNaN(value)) return "-";
  return value.toLocaleString();
};

export default function Traces() {
  const [selectedTraceId, setSelectedTraceId] = useState<string | null>(null);

  const tracesQuery = useQuery({
    queryKey: ["traces"],
    queryFn: fetchTraces,
  });

  const traceDetailQuery = useQuery({
    queryKey: ["trace", selectedTraceId],
    queryFn: () => fetchTrace(selectedTraceId as string),
    enabled: Boolean(selectedTraceId),
  });

  const traces = useMemo<TraceListItemView[]>(() => {
    const items = tracesQuery.data?.data ?? [];
    return items.map((trace) => {
      const tokenInput = null;
      const tokenOutput = null;
      return {
        ...trace,
        latencyText: formatSeconds(trace.latency),
        costText: formatCost(trace.totalCost),
        tokenText: `${formatTokenText(tokenInput)} → ${formatTokenText(tokenOutput)}`,
        status: "success",
      };
    });
  }, [tracesQuery.data]);

  const selectedTrace = traceDetailQuery.data ?? null;

  return (
    <AppLayout>
      <div className="h-full flex flex-col animate-fade-in">
        <div className="mb-4">
          <h1 className="text-2xl font-bold text-foreground">Traces</h1>
          <p className="text-muted-foreground mt-1">View and analyze LLM call chain details</p>
        </div>

        <div className="flex-1 flex gap-4 min-h-0">
          <div className="w-[400px] shrink-0">
            <TraceList
              traces={traces}
              onSelectTrace={(trace) => setSelectedTraceId(trace.id)}
              selectedTraceId={selectedTraceId || undefined}
            />
          </div>

          <div className="flex-1 min-w-0">
            {tracesQuery.isLoading ? (
              <div className="h-full flex items-center justify-center bg-card rounded-lg border border-border">
                <div className="text-sm text-muted-foreground">Loading...</div>
              </div>
            ) : tracesQuery.isError ? (
              <div className="h-full flex items-center justify-center bg-card rounded-lg border border-border">
                <div className="text-sm text-muted-foreground">
                  Failed to load traces. Please check API or Token.
                </div>
              </div>
            ) : selectedTrace ? (
              <TraceDetail trace={selectedTrace as TraceDetailType} />
            ) : (
              <div className="h-full flex items-center justify-center bg-card rounded-lg border border-border">
                <div className="text-center text-muted-foreground">
                  <p className="text-lg">Select a Trace from the list on the left</p>
                  <p className="text-sm mt-1">View detailed call chain information</p>
                </div>
              </div>
            )}
          </div>
        </div>
      </div>
    </AppLayout>
  );
}
