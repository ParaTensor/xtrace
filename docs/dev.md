# XTrace Rust Backend Development Guide

This document guides implementation and development of a "Langfuse-like observability backend" in this repository. External API contracts follow `docs/api.md`.

## Goals and Scope (MVP)

Phase 1 implements:

- Trace list query: `GET /api/public/traces`
- Trace detail query: `GET /api/public/traces/{traceId}`
- Daily aggregated metrics: `GET /api/public/metrics/daily`

Note: For the system to be usable, the backend also needs internal ingest endpoints to write traces/observations to the database. These are not exposed to end users but are required.

## Recommended Tech Stack

- Web framework: Axum (on Tokio)
- Serialization: Serde
- Database: PostgreSQL
- ORM/query: SQLx (compile-time checks recommended)
- Logging and tracing: tracing + tower-http (request logs, trace id)
- Config: Environment variables (optional dotenv)

## Recommended Directory Structure

Backend code can live under `crates/` or at the repo root in a Rust workspace. Suggested layout:

- `crates/xtrace-api/`: HTTP API (routes, DTOs, auth, middleware)
- `crates/xtrace-service/`: Business logic (query, write, aggregation)
- `crates/xtrace-storage/`: SQLx access layer (repo, migrations, model mapping)
- `migrations/`: Database migrations

Alternatively, a monolith can split into `router/`, `service/`, `storage/` within one crate.

## Local Development Dependencies

- Rust stable (>= 1.75 recommended)
- PostgreSQL 14+
- sqlx-cli (optional but strongly recommended for migrations)

Install sqlx-cli (requires postgres dev libs/openssl; usually fine on macOS):

```bash
cargo install sqlx-cli --no-default-features --features native-tls,postgres
```

## Configuration (Environment Variables)

Service startup requires at least:

- `DATABASE_URL`: e.g. `postgres://user:pass@127.0.0.1:5432/xtrace`
- `API_BEARER_TOKEN`: Bearer token for public API (MVP: single token)
- `BIND_ADDR`: Listen address, default `127.0.0.1:8742`

Langfuse public API BasicAuth compatibility (optional):

- `XTRACE_PUBLIC_KEY`
- `XTRACE_SECRET_KEY`

Compatibility:
Also supports legacy names `LANGFUSE_PUBLIC_KEY` / `LANGFUSE_SECRET_KEY`.

Conventions:

- Public endpoints (`/api/public/*`) must validate `Authorization: Bearer <token>`
- Internal ingest endpoints (e.g. `/v1/l/*`) may use a separate token or the same one (simpler initially)

## Database Migrations

Initialize and run migrations (example):

```bash
sqlx database create
sqlx migrate run
```

MVP migrations should include at least:

- `traces` table
- `observations` table
- (Optional) `daily_metrics` table (pre-aggregated)

## Running and Debugging

Local run (example):

```bash
export DATABASE_URL=postgres://user:pass@127.0.0.1:5432/xtrace
export API_BEARER_TOKEN=dev-token
export BIND_ADDR=127.0.0.1:8742
cargo run -p xtrace-api
```

Health check:

- `GET /healthz`: No auth; used for liveness
- `GET /readyz`: No auth; checks PostgreSQL (readiness / orchestrator probe)

## API Implementation Conventions (aligned with docs/api.md)

### Unified Response Structure

Public endpoints follow Langfuse OpenAPI:

`GET /api/public/traces` returns paginated object: `{ data: [...], meta: { page, limit, totalItems, totalPages } }`. Each list item includes Langfuse-aligned fields such as `projectId`, `createdAt`, `updatedAt`, `externalId`, `bookmarked` (see `src/http/traces.rs`).

`GET /api/public/traces/{traceId}` returns the trace object directly (no outer `data/meta/message` wrapper).

### Pagination

`GET /api/public/traces` returns `data.meta`:

- `page`
- `limit`
- `totalItems`
- `totalPages`

### Time Filtering

`from_timestamp` / `to_timestamp` use ISO8601; backend returns 400 on parse failure.

### Tags Filtering

`tags` is an array parameter; semantics are "contains all tags (all-of)".

### Sorting

`order_by` whitelist:

- `timestamp.asc | timestamp.desc`
- `latency.asc | latency.desc`
- `totalCost.asc | totalCost.desc`

Invalid values return 400.

## Minimal Verification (curl)

Examples assume `API_BEARER_TOKEN=dev-token` and service listening on `127.0.0.1:8742`.

```bash
export API_BEARER_TOKEN=dev-token
export BASE_URL=http://127.0.0.1:8742

# For BasicAuth (xinference/langfuse compatibility)
# export XTRACE_PUBLIC_KEY=pk-xxx
# export XTRACE_SECRET_KEY=sk-yyy

# 1) traces list (default: core + io + scores + observations + metrics)
curl -sS "$BASE_URL/api/public/traces?page=1&limit=2" \
  -H "Authorization: Bearer $API_BEARER_TOKEN"

# 2) traces list (fields=core: omits input/output/metadata; includes latency/totalCost; scores/observations empty)
curl -sS "$BASE_URL/api/public/traces?page=1&limit=2&fields=core" \
  -H "Authorization: Bearer $API_BEARER_TOKEN"

# 3) traces list (tags all-of + environment multi-value filter)
curl -sS "$BASE_URL/api/public/traces?page=1&limit=2&tags=foo&tags=bar&environment=default" \
  -H "Authorization: Bearer $API_BEARER_TOKEN"

# 4) trace detail (replace <TRACE_ID> with real UUID)
curl -sS "$BASE_URL/api/public/traces/<TRACE_ID>" \
  -H "Authorization: Bearer $API_BEARER_TOKEN"

# 5) metrics/daily (default last 30 days; supports fromTimestamp/toTimestamp, tags, traceName, userId)
curl -sS "$BASE_URL/api/public/metrics/daily?page=1&limit=50" \
  -H "Authorization: Bearer $API_BEARER_TOKEN"

# 6) traces list (BasicAuth)
curl -sS -u "$XTRACE_PUBLIC_KEY:$XTRACE_SECRET_KEY" \
  "$BASE_URL/api/public/traces?page=1&limit=2"
```

## Ingest Suggestions (Internal Endpoints)

For a complete loop, add internal ingest endpoints (not part of `docs/api.md` public API):

- `POST /v1/l/traces`: Create or upsert trace
- `POST /v1/l/observations`: Create or upsert observation

Key points:

- Idempotent (use `id` or `externalId` as upsert key)
- Allow observation-first or trace-first (at least one ordering must work)

## Testing Suggestions

- Route layer: Use `tower::ServiceExt` for request-level tests
- Storage layer: Use testcontainers or local postgres for integration tests

## Milestones

- M1: Run + auth + DB migrations + ingest writes
- M2: Implement `GET /api/public/traces` and `GET /api/public/traces/{trace_id}`
- M3: Implement `GET /api/public/metrics/daily` (real-time aggregation first; pre-aggregation later)
