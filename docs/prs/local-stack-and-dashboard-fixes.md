# Title

```
fix: live dashboard metrics, CORS for SPA, and traces list query loading
```

# Description

## Summary

Dashboard 与 Traces 列表改为消费真实 xtrace 公共 HTTP API，不再使用前端 mock 统计；并为独立部署的前端启用可配置 CORS，使浏览器预检与鉴权请求能正常通过。

## What changed

### Frontend

- `frontend/src/pages/Dashboard.tsx`：去掉硬编码 mock 指标与图表数据；改为请求 `GET /api/public/metrics`、`GET /api/public/metrics/daily`、`GET /api/public/traces` 渲染总览与趋势；无数据时展示 0 / 空状态。
- `frontend/src/pages/Traces.tsx`：修正 React Query 用法为 `queryFn: () => fetchTraces(n)`。此前直接传入 `fetchTraces` 会把 query context 当成 `limit`，导致错误查询参数与 HTTP 400，列表为空而总览计数仍可能非零。
- `frontend/src/lib/xtrace.ts`：新增 metrics 相关拉取 helper；为 `fetchTraces` 增加安全的数值 `limit` 处理。

### Backend

- `Cargo.toml`：为 `tower-http` 启用 `cors` feature。
- `src/app.rs`：在 Axum 路由上挂载 `CorsLayer`；通过环境变量 `XTRACE_CORS_ORIGINS`（逗号分隔）配置允许的 Origin。
- `src/http/auth.rs`：鉴权与限流中间件跳过 `OPTIONS`，避免 CORS 预检被 401 拒绝。

### Tests

- `tests/integration_test.rs`：删除未使用 import，保持 `clippy -D warnings` 干净。
