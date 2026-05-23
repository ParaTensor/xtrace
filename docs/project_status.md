# xtrace 项目情况快照

**快照日期**：2026-05-23（完成性能与安全优化、可观测性增强、集成测试与 CI 门禁后的例行刷新）

## 定位与范围

xtrace 是一个面向 AI/LLM 应用的可自托管轻量可观测后端，用于采集 trace、observation 与时间序列指标，并对延迟、成本、质量与失败模式提供查询与展示能力。对外 API 与 Langfuse 公共接口保持较高兼容（含 BasicAuth 公钥/私钥模式），并支持会话维度的元数据（`session_id`、`turn_id`、`run_id`、`step_id` 等），详见 `docs/session_ingest.md`。

## 代码与模块布局

服务端为单一 Rust crate（根 `Cargo.toml` 包名 `xtrace`，当前版本 **0.1.0**），入口 `src/main.rs` 读取 `DATABASE_URL`、`API_BEARER_TOKEN` 等环境变量后调用 `run_server`。`src/app.rs` 使用 Axum 挂载路由，启动时执行 `sqlx::migrate!("./migrations")`。异步摄入与指标写入通过 `mpsc` 通道交给 `ingest_worker` 与 `metrics_worker` 后台任务处理。HTTP 层按域拆分在 `src/http/`（`traces`、`metrics`、`auth`、`projects`、`ops` 等），摄入逻辑在 `src/ingest/`（含 `batch` 与 `otlp` protobuf 解码路径）。
工作区成员包含 **`crates/xtrace-client`**（同为 0.1.0），提供 HTTP 客户端与可选的 `tracing` feature（`XtraceLayer` 自动上报指标与 span 时长）。
前端仪表板位于 **`frontend/`**，技术栈为 Vite + React 18 + TypeScript + shadcn/ui + TanStack Query + Recharts，通过 `VITE_XTRACE_BASE_URL` 与 `VITE_XTRACE_API_TOKEN` 指向后端。
面向用户的文档站点在 **`www/`**，使用 VitePress（`xtrace-docs`），由独立工作流部署到 Cloudflare Pages。
数据库迁移当前为三条：`0001_init.sql`、`0002_add_environment.sql`、`0003_add_metrics.sql`，覆盖初始化表结构、环境与指标相关演进。
脚本与 SDK 样例包括 `scripts/`（如 Python `pyproject.toml` 与校验脚本）、`README.md` 中的 curl 与 Rust 示例。

## 运维与 CI

`.github/workflows/deploy.yml`（名称显示为 CI）在 `push`/`pull_request` 与 `workflow_dispatch` 下执行 `cargo fmt --check`、`cargo clippy -D warnings`、`cargo test --all`。对 `main` 的 push 会忽略仅变更 `www/**`、`docs/**`、全库 `**.md` 及文档部署工作流的路径，避免无 Rust 变更时跑全套 CI。
此外，CI 流程新增了对前端的门禁校验（`npm run lint / test / build`）以及在 Rust Job 中配置了 PostgreSQL 16 数据库服务和 `DATABASE_URL`，以确保集成测试能在 CI 中完整运行。
`.github/workflows/deploy-docs.yml` 在 `www/**` 或该工作流本身变更时构建 VitePress 并 `wrangler pages deploy` 到项目 `xtrace-docs`，依赖仓库密钥 `CLOUDFLARE_API_TOKEN` 与 `CLOUDFLARE_ACCOUNT_ID`。
`.github/workflows/project-status-reminder.yml` 按周触发（可 `workflow_dispatch` 手动运行），仅在 Actions 界面提示维护者根据 `AGENTS.md` 更新本文件；它不自动改写仓库内容。

## 本地验证（本次执行）

在审查机器上执行了单元测试与集成测试：
- 单元测试（9个）全部通过：包含 Auth 解析单元测试（4个）和 Trace `fields`/`order_by` 筛选排序解析单元测试（5个）。
- 集成测试（5个）在 `tests/integration_test.rs` 中完整跑通：覆盖了 `healthz` 免密访问、受保护端点的鉴权逻辑、Ingest-Read 链路往返、多项目 project_id 隔离安全性、以及 Metrics 的批量上报与时序查询。

## 文档索引（仓库内）

除本快照外，`docs/` 下还有 API 契约（`api.md`）、摄入与会话设计（`ingest.md`、`session_ingest.md`、`trace_and_session_design.md`）、与外部系统对接的分析稿（如 Nebula、Xinference、Zene 等）、**`xinference_integration.md`（Xinference 生产对接）**，以及 `dev.md` 中的后端开发约定与里程碑建议。`README.md` 汇总运行方式、环境变量与主要 HTTP 端点。

近期后端增强（面向生产集成）：
- **性能与安全优化**：
  - `get_trace` 和 `get_observations` 增加了 `project_id` 物理隔离，杜绝跨项目读取 Trace 的安全隐患。
  - 在获取 Trace 列表时，若请求指定 `fields=core`，查询中将彻底排除 input、output、metadata 等大 JSON 字段的查询，并不进行 observation 子查询，显著降低内存和数据库带宽开销。
  - 重构了 Metrics 查询的 SQL，引入窗口函数（`ROW_NUMBER()` 和 `DENSE_RANK()`）直接在数据库端限制 series 和 points 数量（最多 50 条 series，每条最多 1000 个数据点），避免了先在数据库查出全量数据再在内存里截断的低效方案。
  - 将 Ingest worker 从单条循环插入重构为 Bulk Upsert，极大地减少了数据库的 Round-trip 次数，提升了写入吞吐量。
- **可观测性增强**：
  - 新增 `IngestStats` 指标统计（包括入队数、队列拒绝数、写库成功数、写库失败数、成功写入条目数）。
  - 新增内部管理端点 `GET /api/internal/ingest_stats`，支持监控当前的摄入状态和吞吐指标。

## 风险与后续关注点（静态审查结论）

1. 测试已经补充了初步的集成测试与单元测试，但对于超高并发/超大规模数据包的高负载场景，仍需根据生产中 `IngestStats` 进行监控和调优（如背压调整等）。
2. 在前端或后端进行协议层字段变动时，需要及时核对并维护 `docs/api.md` 以确保文档与实际代码的一致性。