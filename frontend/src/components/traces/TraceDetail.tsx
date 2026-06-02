import { useMemo, useState } from "react";
import { cn } from "@/lib/utils";
import {
  Copy,
  MessageSquare,
  ChevronDown,
  ChevronRight,
  Sparkles,
  ArrowRightLeft,
  Database,
  Code,
  Wrench,
  Brain,
  Search,
  Hand,
} from "lucide-react";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import type { Observation, TraceDetail } from "@/lib/xtrace";

interface SpanNode {
  id: string;
  name: string;
  type: "generation" | "retrieval" | "span" | "embedding" | "observation";
  stepType?: string;
  latencyText: string;
  costText?: string;
  tokensText?: string;
  children?: SpanNode[];
  level?: string | null;
}

interface TraceDetailProps {
  trace: TraceDetail;
}

const formatSeconds = (value: number | null | undefined) => {
  if (value == null || !Number.isFinite(value) || value < 0) return "-";
  return `${value.toFixed(2)}s`;
};

const formatCost = (value: number | null) => {
  if (value === null || Number.isNaN(value)) return "-";
  return `$${value.toFixed(6)}`;
};

const formatTokens = (obs: Observation) => {
  if (!obs.usage) return "-";
  return `${obs.usage.input} → ${obs.usage.output}`;
};

const observationToNode = (observation: Observation): SpanNode => {
  const type = observation.type.toLowerCase();
  const metadata = (observation.metadata as Record<string, any>) || {};
  const stepType = metadata.step_type || metadata.stepType;

  const mappedType = ((): SpanNode["type"] => {
    if (type.includes("generation")) return "generation";
    if (type.includes("embedding")) return "embedding";
    if (type.includes("retrieval")) return "retrieval";
    return "observation";
  })();

  return {
    id: observation.id,
    name: observation.name || observation.type,
    type: mappedType,
    stepType: typeof stepType === "string" ? stepType.toLowerCase() : undefined,
    latencyText: formatSeconds(observation.latency),
    costText: formatCost(observation.calculatedTotalCost ?? observation.totalPrice),
    tokensText: observation.usage ? formatTokens(observation) : undefined,
    level: observation.level,
  };
};

const buildSpanTree = (observations: Observation[]): SpanNode[] => {
  const nodes = new Map<string, SpanNode>();
  const childrenMap = new Map<string, string[]>();
  const roots: SpanNode[] = [];

  observations.forEach((obs) => {
    nodes.set(obs.id, observationToNode(obs));
    if (obs.parentObservationId) {
      const list = childrenMap.get(obs.parentObservationId) || [];
      list.push(obs.id);
      childrenMap.set(obs.parentObservationId, list);
    }
  });

  observations.forEach((obs) => {
    const node = nodes.get(obs.id);
    if (!node) return;
    const childIds = childrenMap.get(obs.id) || [];
    if (childIds.length) {
      node.children = childIds
        .map((childId) => nodes.get(childId))
        .filter((child): child is SpanNode => Boolean(child));
    }
    if (!obs.parentObservationId) {
      roots.push(node);
    }
  });

  return roots.length ? roots : observations.map(observationToNode);
};

function SpanTreeNode({ node, depth = 0 }: { node: SpanNode; depth?: number }) {
  const [expanded, setExpanded] = useState(true);
  const hasChildren = node.children && node.children.length > 0;

  const getTypeIcon = (node: SpanNode) => {
    // Priority: Specific Step Type > General Observation Type
    if (node.stepType) {
      switch (node.stepType) {
        case "tool_call":
        case "tool":
          return <Wrench className="h-3.5 w-3.5 text-xtrace-orange" />;
        case "retrieval":
          return <Search className="h-3.5 w-3.5 text-xtrace-info" />;
        case "planner":
        case "plan":
          return <Brain className="h-3.5 w-3.5 text-xtrace-purple" />;
        case "memory_write":
        case "memory":
          return <Database className="h-3.5 w-3.5 text-muted-foreground" />;
        case "handoff":
          return <Hand className="h-3.5 w-3.5 text-muted-foreground" />;
        case "llm_call":
          return <Sparkles className="h-3.5 w-3.5 text-xtrace-pink" />;
      }
    }

    // Fallback to basic types
    switch (node.type) {
      case "generation":
        return <Sparkles className="h-3.5 w-3.5 text-xtrace-pink" />;
      case "retrieval":
        return <ArrowRightLeft className="h-3.5 w-3.5 text-xtrace-info" />;
      case "embedding":
        return <Database className="h-3.5 w-3.5 text-xtrace-orange" />;
      default:
        return <Code className="h-3.5 w-3.5 text-muted-foreground" />;
    }
  };

  return (
    <div className="select-none">
      <div
        className={cn(
          "flex items-center gap-2 py-1.5 px-2 rounded-md hover:bg-accent/50 cursor-pointer group",
          depth > 0 && "ml-5"
        )}
        onClick={() => hasChildren && setExpanded(!expanded)}
      >
        <div className="w-4 h-4 flex items-center justify-center">
          {hasChildren ? (
            expanded ? (
              <ChevronDown className="h-3.5 w-3.5 text-muted-foreground" />
            ) : (
              <ChevronRight className="h-3.5 w-3.5 text-muted-foreground" />
            )
          ) : (
            <div className="w-1 h-1 rounded-full bg-muted-foreground/30" />
          )}
        </div>

        {getTypeIcon(node)}

        <span className="font-medium text-sm text-foreground">{node.name}</span>

        {node.level && (
          <Badge variant="outline" className="text-[10px] h-4 px-1">
            {node.level}
          </Badge>
        )}

        <div className="ml-auto flex items-center gap-3 text-xs text-muted-foreground">
          <span>{node.latencyText}</span>
          {node.tokensText && <span>{node.tokensText}</span>}
          {node.costText && <span>{node.costText}</span>}
        </div>
      </div>

      {hasChildren && expanded && (
        <div className="border-l border-border ml-4">
          {node.children!.map((child) => (
            <SpanTreeNode key={child.id} node={child} depth={depth + 1} />
          ))}
        </div>
      )}
    </div>
  );
}

export function TraceDetail({ trace }: TraceDetailProps) {
  const spanRoots = useMemo(() => buildSpanTree(trace.observations), [trace.observations]);
  
  // Extract IDs from metadata for display
  const metadata = (trace.metadata as Record<string, any>) || {};
  const turnId = metadata.turn_id || metadata.turnId;
  const runId = metadata.run_id || metadata.runId;

  return (
    <div className="flex flex-col h-full bg-card rounded-lg border border-border overflow-hidden">
      <div className="p-4 border-b border-border">
        <div className="flex items-center justify-between">
          <div className="flex items-center gap-3">
            <Code className="h-5 w-5 text-muted-foreground" />
            <span className="font-semibold text-foreground">{trace.name || "(unnamed)"}</span>
            <button className="text-muted-foreground hover:text-foreground">
              <Copy className="h-3.5 w-3.5" />
            </button>
            <span className="text-xs text-muted-foreground">ID</span>
          </div>
          <div className="flex items-center gap-2">
            <Button variant="ghost" size="icon" className="h-8 w-8">
              <MessageSquare className="h-4 w-4" />
            </Button>
          </div>
        </div>

        <div className="mt-3 text-sm text-muted-foreground">{trace.timestamp || "-"}</div>

        <div className="mt-3 flex flex-wrap gap-2">
          {trace.sessionId && (
            <Badge className="bg-xtrace-info/10 text-xtrace-info border-xtrace-info/20 hover:bg-xtrace-info/20">
              Session: {trace.sessionId}
            </Badge>
          )}
          {turnId && (
            <Badge variant="outline" className="border-dashed">
              Turn: {turnId}
            </Badge>
          )}
          {runId && (
            <Badge variant="outline" className="border-dashed">
              Run: {runId}
            </Badge>
          )}
          {trace.userId && (
            <Badge className="bg-xtrace-purple/10 text-xtrace-purple border-xtrace-purple/20 hover:bg-xtrace-purple/20">
              User ID: {trace.userId}
            </Badge>
          )}
          <Badge variant="outline">Env: {trace.projectId || "-"}</Badge>
          <Badge variant="outline">Latency: {formatSeconds(trace.latency)}</Badge>
          <Badge variant="outline">Total Cost: {formatCost(trace.totalCost)}</Badge>
        </div>

        <div className="mt-3 flex items-center gap-2 text-xs">
          <span className="text-muted-foreground">
            Observations: {trace.observations.length}
          </span>
          {trace.release && (
            <>
              <span className="text-muted-foreground">|</span>
              <span className="text-muted-foreground font-mono text-[10px]">
                Release: {trace.release}
              </span>
            </>
          )}
        </div>
      </div>

      <div className="flex flex-1 overflow-hidden">
        <div className="w-[380px] border-r border-border overflow-y-auto scrollbar-thin p-3">
          {spanRoots.length ? (
            spanRoots.map((node) => <SpanTreeNode key={node.id} node={node} />)
          ) : (
            <div className="text-sm text-muted-foreground">No Observations</div>
          )}
        </div>

        <div className="flex-1 overflow-y-auto scrollbar-thin">
          <Tabs defaultValue="preview" className="h-full flex flex-col">
            <div className="border-b border-border px-4">
              <TabsList className="h-10 bg-transparent p-0 gap-4">
                <TabsTrigger
                  value="preview"
                  className="data-[state=active]:bg-transparent data-[state=active]:shadow-none data-[state=active]:border-b-2 data-[state=active]:border-primary rounded-none px-0 pb-2"
                >
                  Preview
                </TabsTrigger>
                <TabsTrigger
                  value="scores"
                  className="data-[state=active]:bg-transparent data-[state=active]:shadow-none data-[state=active]:border-b-2 data-[state=active]:border-primary rounded-none px-0 pb-2"
                >
                  Scores
                </TabsTrigger>
              </TabsList>
              <div className="absolute right-4 top-2 flex items-center gap-2">
                <Button variant="ghost" size="sm" className="h-7 text-xs" disabled>
                  Formatted
                </Button>
                <Button variant="ghost" size="sm" className="h-7 text-xs" disabled>
                  JSON
                </Button>
              </div>
            </div>

            <TabsContent value="preview" className="flex-1 p-4 m-0">
              <div className="space-y-4">
                <div>
                  <div className="flex items-center justify-between mb-2">
                    <h3 className="font-medium text-foreground">Input</h3>
                  </div>
                  <div className="bg-muted rounded-lg p-3 font-mono text-sm">
                    <pre className="text-xs whitespace-pre-wrap">
                      {JSON.stringify(trace.input, null, 2) || "-"}
                    </pre>
                  </div>
                </div>

                <div>
                  <div className="flex items-center justify-between mb-2">
                    <h3 className="font-medium text-foreground">Output</h3>
                  </div>
                  <div className="bg-accent/50 border-l-2 border-primary rounded-lg p-3 text-sm leading-relaxed">
                    <pre className="text-xs whitespace-pre-wrap">
                      {JSON.stringify(trace.output, null, 2) || "-"}
                    </pre>
                  </div>
                </div>

                <div>
                  <h3 className="font-medium text-foreground mb-2">Metadata</h3>
                  <div className="bg-muted rounded-lg p-3 font-mono text-sm">
                    <pre className="text-xs whitespace-pre-wrap">
                      {JSON.stringify(trace.metadata, null, 2) || "-"}
                    </pre>
                  </div>
                </div>
              </div>
            </TabsContent>

            <TabsContent value="scores" className="flex-1 p-4 m-0">
              <div className="flex items-center justify-center h-full text-muted-foreground">
                No score data
              </div>
            </TabsContent>
          </Tabs>
        </div>
      </div>
    </div>
  );
}
