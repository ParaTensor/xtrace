# 面向自动化助手与维护者的说明

本仓库是 **xtrace**：自托管的 AI/LLM 可观测后端（Rust + PostgreSQL），带 React 仪表板（`frontend/`）与 VitePress 文档站（`www/`）。权威运行说明见根目录 `README.md`，后端实现约定见 `docs/dev.md`，公共 HTTP 契约见 `docs/api.md`。

## 产品边界与集成原则

xtrace 的定位是 **通用的 AI/LLM observability substrate**：负责接收、存储、查询与展示 traces、observations、metrics，以及少量通用关联元数据。它可以提供统一的 OTLP / HTTP ingest、公共查询接口、基础时序聚合、跨服务 trace 关联能力，但**不是**上层业务系统自身的调度面、路由面、执行面或诊断控制面。

边界上遵守以下原则：

- **保留在 xtrace 核心中的能力**：标准化上下文传播契约、通用 trace/span/observation 关联、通用 streaming lifecycle 观测原语、通用 metrics ingest/query、通用 metadata 存储与检索。
- **保留在业务系统中的能力**：业务路由与调度策略、节点或引擎生命周期管理、领域特定诊断规则、专属运维解释逻辑、以及仅对单一系统有意义的 adapter / shim / queue 回传实现。
- **通过命名约定承载领域语义**：领域事件、metadata 和 metric labels 应优先通过命名约定表达，而不是在 xtrace 核心 schema 中固化为专属顶层字段、专属表结构或专属 API。
- **避免破坏通用性**：不要为单一集成方在核心接口中引入专属数据模型、默认高频落库策略、或与现有兼容面冲突的行为。

如果一个需求要求 xtrace 直接理解某个业务系统的内部状态机、调度语义或运行时细节，默认应将这部分能力保留在业务系统自身实现，而不是放进 xtrace 核心。

修改服务端行为时优先阅读 `src/app.rs` 的路由装配与 `src/http/`、`src/ingest/` 中的现有模式；数据库变更必须新增 `migrations/` 下的顺序迁移并在有数据库的环境下验证 `sqlx migrate`。不要在没有需求时改动 `www/` 与 `frontend/` 的依赖大版本。合并前在本地执行 `cargo fmt --all --check`、`cargo clippy --all-targets --all-features -- -D warnings`、`cargo test --all`，前端改动则在 `frontend/` 内执行 `npm run lint` 与 `npm run build`（若触及 UI）。

## 定期更新项目情况（核心例行工作）

维护者或 Cursor Agent 应**至少每两周一次**（发布前或大合并后则应立刻执行）做一次「项目情况」刷新，并把结论写回 **`docs/project_status.md`**。

执行步骤：检出当前默认分支并 `git pull`；用 `git log --oneline -20` 与 `git diff`（相对上一快照若有标签或自行记录的提交）了解近期变更；查看根 `Cargo.toml` 与 `crates/xtrace-client/Cargo.toml` 的版本号、`migrations/` 是否新增、`.github/workflows/` 是否有行为变化；在本地运行 `cargo test --all`（以及你改动的子项目相关命令）；打开 `README.md` 核对环境变量与端点描述是否仍与代码一致。

更新 `docs/project_status.md` 时：改写文首「快照日期」为当天（使用用户或 CI 环境中的权威日期）；用简短段落概括架构、CI、迁移与文档站现状；如实记录测试数量与已知缺口；删除已过时的表述而非堆叠历史段落。若本次仅做例行刷新且没有功能变更，仍应更新「快照日期」并在文末用一两句话说明「本期无结构性变更」或列出刚合并的主题。

该例行工作的目标是让新参与者或 Agent 在五分钟以内从单一文件了解仓库边界、运行方式、自动化门禁与当前风险，而不是替代 `README.md` 或详细设计文档。

## 定时提醒（可选）

仓库提供 `.github/workflows/project-status-reminder.yml`：默认在每周一 UTC 09:00 运行，在 GitHub Actions 摘要中打印维护提示，**不会**自动修改 `docs/project_status.md`。若组织不需要该提醒，可在 GitHub 仓库设置中禁用该工作流或删除该文件。
