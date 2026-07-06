# xtrace 指标能力与 OTLP metrics 实施路线（X0–X3）

> 背景：作为让上层 LLM 服务（如 Xinference/PowerLLM）**去 Langfuse 化**、由 xtrace 承接 trace + generation + 用量/成本 + LLM 业务指标的一部分，本文规划 xtrace 侧需要补齐/增强的能力。
> 性质：**实施规划文档，本身不含代码改动。** 具体落地须按 `AGENTS.md`：改路由先读 `src/app.rs` 装配与 `src/http/`、`src/ingest/` 现有模式；DB 变更新增顺序 `migrations/` 并验证 `sqlx migrate`；合并前跑 `cargo fmt --all --check`、`cargo clippy --all-targets --all-features -- -D warnings`、`cargo test --all`。
> **边界原则**（`AGENTS.md`）：xtrace 是**通用 observability substrate**。以下能力均保持通用——领域语义（模型/引擎/GPU 等）**通过命名约定与 labels 承载**，不在核心 schema 固化业务专属字段或专属表。

---

## 0. 现状快照

- **指标存储**：单一通用点表 `metrics(id, project_id, environment, name, labels JSONB, value, timestamp, created_at)`（`migrations/0003_add_metrics.sql`），索引 `(project_id,name,timestamp)`、labels GIN、`timestamp`。
- **写入**：`POST /v1/metrics/batch`（`src/app.rs:38`，handler `src/http/metrics.rs`），队列 + ~50ms 微批 bulk insert。DTO `MetricPointIngest { name, labels, value, timestamp }`。
- **查询**：`GET /api/public/metrics/query`（`name` + `from/to` + `labels` + `step`(1m/5m/1h/1d) + `agg`(avg/max/min/sum/last/**p50/p90/p99**) + `group_by`）；`GET /api/public/metrics/names`；`GET /api/public/metrics`（traces 概览:count、avg/p95/p99 latency、error count）。分位数由 `percentile_cont`(PG) / 内存实现对**原始点**计算。
- **OTLP**：仅 traces —— `POST /api/public/otel/v1/traces`（`src/app.rs:40`，`src/ingest/otlp.rs:post_otel_traces`，支持 JSON / `application/x-protobuf` / gzip）。**无 OTLP metrics / logs**。
- **认证/部署**：Bearer(写) + BasicAuth(Langfuse 兼容读)；内存+JSON 零依赖模式 与 PostgreSQL 双存储；per-token 限流；`project_id` 现按单默认项目运行。

---

## X0 · 新增 OTLP metrics 摄入（核心）

**目标**：接收标准 OpenTelemetry metrics，使上层服务可用 OTel SDK 同时把 trace 与 metric 打进 xtrace，减少对 bespoke `/v1/metrics/batch` 与 Langfuse SDK 的耦合。

**接口**：新增 `POST /api/public/otel/v1/metrics`，与 traces 端点对称：
- 复用 `src/ingest/otlp.rs` 既有解码管线（`ungzip_if_needed`、content-type 分支、JSON 与 `application/x-protobuf` 两条路径、`pb_to_otel_json` 风格转换）。
- 解析 `ExportMetricsServiceRequest`（`resource_metrics → scope_metrics → metrics`）。
- 走与 traces 相同的队列摄入（`state.ingest_tx.try_send`，Full→429、Closed→503）。

**映射到现有点模型（不改核心 schema）**：
- **Gauge / Sum(NumberDataPoint)**：每个数据点 → 一行 `metrics`，`name`=metric name，`value`=数据点值，`timestamp`=数据点时间，`labels`= 数据点 attributes。
- **Histogram**：为保持「点模型 + 查询期聚合」，落派生点：`{name}_count`、`{name}_sum`（必要时 `{name}_bucket` 带 `le` label）。文档需注明：xtrace 的分位数是对**原始样本点**求真实分位（`percentile_cont`），与 Prometheus histogram 的**近似分位**语义不同——建议延迟类指标优先以**原始点**上报（见 X1）。
- **labels 来源**：合并 Resource attributes（如 `service.name`）、Scope、DataPoint attributes；对齐 **OTel GenAI 语义约定**（`gen_ai.*`，如 `gen_ai.request.model`、`gen_ai.usage.input_tokens`）——**通过命名约定承载**，不新增专属列。
- `project_id` / `environment` 沿用 traces 端点的既有推导方式。

**产物**：新增 `src/ingest/otlp.rs`(或 `otlp_metrics.rs`) 的 metrics 解析 + `map_otel_metrics_to_points`；`src/app.rs` 注册路由；proto 依赖复用现有 OTLP protobuf 引入。**无需新迁移**（复用 `metrics` 表）。

**测试**：`tests/` 下补 JSON 与 protobuf 两种 payload 的摄入用例 + 查询回读断言。

---

## X1 · 指标查询与语义增强

- **补聚合**：在 `get_metrics_query` 增加 `count`、以及由累积计数点推导的 `rate/increase`（可选，注明与 counter reset 的处理）。
- **多维 group_by**：允许按多个 label key 分组（现为单 key）。
- **语义文档化**：在 `docs/api.md` 明确「点即原始样本」——TTFT/时延等按**每请求一个点**上报时，`agg=p99` 为真实分位；直方图派生点仅用于兼容 OTLP Histogram 输入。
- **概览端点澄清**：文档区分 `/api/public/metrics`（traces 概览分析）与 `/api/public/metrics/query`（通用时序），避免使用者混淆。

---

## X2 · 基数治理与多租户

- **高基数保护**（通用，非针对单一集成方）：ingest 侧对 label key 提供可配置白名单/维度上限与拒绝或丢弃策略；查询侧已有 `truncated` 元信息，补显式 series cap 与超限行为约定。防止 `metrics` 表序列爆炸（例如上层误把 `user_id`/`request_id` 放进 labels）。
- **多租户**：schema 已具 `project_id`；若要支撑多部署/商业化，规划真正的多 project 鉴权与隔离(而非单默认项目)，保持与现有 BasicAuth/Bearer 兼容面不冲突。

---

## X3 · 自托管 / 商业化打磨

- **数据保留 / TTL / 降采样**：为 `metrics`(及 traces/observations) 增加保留期与滚动降采样(长期存储成本);内存+JSON 模式补大小上限与淘汰。
- **读写 token 分离**：区分只读查询 token 与写入 token(现 Bearer 写 + BasicAuth 读的基础上细化)。
- **备份/迁移**：PostgreSQL 备份与 JSON 模式导入导出说明,补进 `README.md`/`docs/dev.md`。

---

## 与上层服务的契约（对齐命名约定）

上层服务（如 PowerLLM）应遵守，以保持 xtrace 通用性：
- 延迟类（TTFT、端到端时延）以**每请求一个原始点**上报，便于 `p50/p90/p99` 真实分位。
- labels 仅放**低基数**维度（如 `model`、`model_type`、`format`、`quantization`、`node`）；**不要**把 `user_id`/`request_id` 等高基数值放进 metric labels（用户/请求维度归 trace/generation 与 metadata）。
- 命名对齐 OTel GenAI 语义约定（`gen_ai.*`）。
- 基础设施指标（GPU/CPU/节点 scrape）留在业务侧的 Prometheus 生态；xtrace 承接 LLM 业务/用量/成本与由服务主动推送的关键点。

> 关联文档：`docs/xinference_integration.md`、`docs/xtrace_langfuse_python_compat.md`、`docs/session_ingest.md`、`docs/api.md`、`docs/dev.md`；上层（PowerLLM）侧总体方案见 powerllm 仓库 `docs/xtrace_observability_plan.md`。

---

## 优先级小结

| 项 | 内容 | 边界 | 建议 |
|----|------|------|------|
| X0 | OTLP metrics 摄入端点 + 映射到点模型 | 通用 | **先做**（打通 OTel 底座） |
| X1 | 查询聚合增强 + 语义文档化 | 通用 | 紧随其后 |
| X2 | 基数治理 + 多租户 | 通用 | 视规模/商业化 |
| X3 | 保留/降采样、token 分离、备份 | 通用 | 自托管/商业化前 |
