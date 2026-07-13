export type TraceListItem = {
  id: string;
  timestamp: string | null;
  name: string | null;
  input: unknown | null;
  output: unknown | null;
  sessionId: string | null;
  release: string | null;
  version: string | null;
  userId: string | null;
  metadata: Record<string, unknown> | null;
  tags: string[] | null;
  public: boolean;
  htmlPath: string | null;
  latency: number | null;
  totalCost: number | null;
  observations: string[];
  scores: string[] | null;
  externalId: string | null;
  bookmarked: boolean;
  projectId: string | null;
  createdAt: string | null;
  updatedAt: string | null;
};

export type Observation = {
  id: string;
  traceId: string;
  type: string;
  name: string | null;
  startTime: string | null;
  endTime: string | null;
  completionStartTime: string | null;
  model: string | null;
  modelParameters: Record<string, unknown> | null;
  input: unknown | null;
  version: string | null;
  metadata: Record<string, unknown> | null;
  output: unknown | null;
  usage: {
    input: number;
    output: number;
    total: number;
    unit: string;
  } | null;
  level: string | null;
  statusMessage: string | null;
  parentObservationId: string | null;
  promptId: string | null;
  promptName: string | null;
  promptVersion: string | null;
  modelId: string | null;
  inputPrice: number | null;
  outputPrice: number | null;
  totalPrice: number | null;
  calculatedInputCost: number | null;
  calculatedOutputCost: number | null;
  calculatedTotalCost: number | null;
  latency: number | null;
  timeToFirstToken: number | null;
  completionTokens: number | null;
  unit: string | null;
};

export type TraceDetail = Omit<TraceListItem, "observations"> & {
  observations: Observation[];
};

export type TraceListResponse = {
  data: TraceListItem[];
  meta: {
    page: number;
    limit: number;
    totalItems: number;
    totalPages: number;
  };
};

export type TraceDetailResponse = TraceDetail;

export type MetricsOverviewRow = {
  count_count: number;
  avg_latency: number;
  p95_latency: number;
  p99_latency: number;
  error_count: number;
};

export type MetricsOverviewResponse = {
  data: MetricsOverviewRow[];
};

export type MetricsDailyItem = {
  date: string;
  countTraces?: number;
  countObservations?: number;
  totalCost?: number | null;
  totalTokens?: number | null;
  [key: string]: unknown;
};

export type MetricsDailyResponse = {
  data: MetricsDailyItem[];
  meta?: {
    page: number;
    limit: number;
    totalItems: number;
    totalPages: number;
  };
};

const DEFAULT_BASE_URL = "http://127.0.0.1:3000";

export const getBaseUrl = () =>
  import.meta.env.VITE_XTRACE_BASE_URL || DEFAULT_BASE_URL;

export const getAuthToken = () => import.meta.env.VITE_XTRACE_API_TOKEN || "";

const buildHeaders = () => {
  const token = getAuthToken();
  return token ? { Authorization: `Bearer ${token}` } : {};
};

export async function fetchTraces(limit = 50) {
  const baseUrl = getBaseUrl();
  // Guard: react-query may pass QueryFunctionContext as the first argument when
  // `queryFn: fetchTraces` is used directly — never stringify that into `limit=`.
  const safeLimit =
    typeof limit === "number" && Number.isFinite(limit)
      ? Math.min(200, Math.max(1, Math.floor(limit)))
      : 50;
  const response = await fetch(
    `${baseUrl}/api/public/traces?page=1&limit=${safeLimit}&fields=core`,
    {
      headers: buildHeaders(),
    },
  );
  if (!response.ok) {
    throw new Error(`Failed to load traces (${response.status})`);
  }
  return (await response.json()) as TraceListResponse;
}

export async function fetchTrace(traceId: string) {
  const baseUrl = getBaseUrl();
  const response = await fetch(
    `${baseUrl}/api/public/traces/${traceId}?resolveMedia=true&resolveWith=base64DataUri`,
    {
      headers: buildHeaders(),
    },
  );
  if (!response.ok) {
    throw new Error(`Failed to load trace (${response.status})`);
  }
  return (await response.json()) as TraceDetailResponse;
}

/** Langfuse-compatible overview: GET /api/public/metrics?query=... */
export async function fetchMetricsOverview(
  view: "traces" | "observations" = "traces",
) {
  const baseUrl = getBaseUrl();
  const query = JSON.stringify({
    view,
    metrics: [{ measure: "count", aggregation: "count" }],
  });
  const response = await fetch(
    `${baseUrl}/api/public/metrics?query=${encodeURIComponent(query)}`,
    { headers: buildHeaders() },
  );
  if (!response.ok) {
    throw new Error(`Failed to load metrics overview (${response.status})`);
  }
  return (await response.json()) as MetricsOverviewResponse;
}

export async function fetchMetricsDaily(limit = 30) {
  const baseUrl = getBaseUrl();
  const response = await fetch(
    `${baseUrl}/api/public/metrics/daily?page=1&limit=${limit}`,
    { headers: buildHeaders() },
  );
  if (!response.ok) {
    throw new Error(`Failed to load metrics daily (${response.status})`);
  }
  return (await response.json()) as MetricsDailyResponse;
}
