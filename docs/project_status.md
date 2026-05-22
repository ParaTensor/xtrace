# xtrace 项目情况快照

**快照日期**：2026-05-22（完成 XtraceLayer 并发优化与前端 API 字段结构对齐后的例行刷新）

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

`.github/workflows/deploy-docs.yml` 在 `www/**` 或该工作流本身变更时构建 VitePress 并 `wrangler pages deploy` 到项目 `xtrace-docs`，依赖仓库密钥 `CLOUDFLARE_API_TOKEN` 与 `CLOUDFLARE_ACCOUNT_ID`。

`.github/workflows/project-status-reminder.yml` 按周触发（可 `workflow_dispatch` 手动运行），仅在 Actions 界面提示维护者根据 `AGENTS.md` 更新本文件；它不自动改写仓库内容。

## 本地验证（本次执行）

在审查机器上对仓库执行了 `cargo test --all`，编译与测试阶段成功退出；当前工作区内 Rust 侧**未定义任何 `#[test]`**，因此结果为「0 tests passed」——若需回归保障，后续可在 HTTP 层或存储层补充集成测试。

## 文档索引（仓库内）

除本快照外，`docs/` 下还有 API 契约（`api.md`）、摄入与会话设计（`ingest.md`、`session_ingest.md`、`trace_and_session_design.md`）、与外部系统对接的分析稿（如 Nebula、Xinference、Zene 等）、**`xinference_integration.md`（Xinference 生产对接）**，以及 `dev.md` 中的后端开发约定与里程碑建议。`README.md` 汇总运行方式、环境变量与主要 HTTP 端点。

近期后端增强（面向生产集成）：`GET /readyz`（数据库就绪探针）、`DefaultBodyLimit` 限制摄入请求体、默认关闭未配置 Langfuse 密钥时的匿名兼容路径（`XTRACE_ALLOW_UNAUTHENTICATED_COMPAT`）、trace 列表响应补充 `projectId` / `createdAt` 等与 Langfuse 对齐的字段。v0.1.0 这轮还补充了 Xinference 对接文档、GCP/systemd 部署样例、联调脚本，以及 batch ingest 对 `sessionId` 的兼容别名解析。在最近的优化中，修复了前端 React 应用与后端 API 契约的类型及字段对齐问题（去除了包裹 traces/trace 的外层 `{ message, data }` 包装，直接对接扁平 JSON），以及重构了 SDK `XtraceLayer` 内部追踪记录的数据存储方案，将其从基于全局 `Mutex<Vec<SpanRecord>>` 线性查找的慢路径改为基于 `tracing` 的 `ExtensionsMut` 实现 O(1) 锁自由的按 Span 扩展绑定，极大地提升了高并发场景下的性能并降低了锁竞争。同时进行了数据库性能优化：重构了分页接口的 SQL 结构，采用子查询对 `traces` 先分页再在外层使用相关子查询关联 `observations` IDs，避免了全量 `LEFT JOIN` 与 `GROUP BY`；新增了顺序迁移 `0004_add_parent_observation_index.sql` 为 `observations(parent_observation_id)` 补充部分索引以大幅加速 Trace 树解析。

## 风险与后续关注点（静态审查结论）

测试覆盖偏薄，主要依赖 fmt/clippy 与人工联调；指标与摄入异步队列在高压下的背压与可观测性需在真实负载下验证。文档站点与 `docs/` 目录内容不同步触发同一套 CI，合并文档类变更时建议仍本地或手动跑一遍 Rust CI。若对外发布版本，需同时核对根 crate 与 `xtrace-client` 的版本号及 `README` 中的引用是否一致。
