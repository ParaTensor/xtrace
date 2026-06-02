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

const DEFAULT_BASE_URL = "http://127.0.0.1:8742";

export const getBaseUrl = () =>
  import.meta.env.VITE_XTRACE_BASE_URL || DEFAULT_BASE_URL;

export const getAuthToken = () => import.meta.env.VITE_XTRACE_API_TOKEN || "";

const buildHeaders = () => {
  const token = getAuthToken();
  return token ? { Authorization: `Bearer ${token}` } : {};
};

export async function fetchTraces() {
  const baseUrl = getBaseUrl();
  const response = await fetch(
    `${baseUrl}/api/public/traces?page=1&limit=50&fields=core`,
    {
      headers: buildHeaders(),
    }
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
