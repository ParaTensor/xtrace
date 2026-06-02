# Nebula 集成边界与 xtrace 通用优化项

本文档记录 xtrace 与 Nebula 在职责边界上的对齐结论，以及在该边界下 xtrace 仍值得推进的通用优化项。

核心原则是：**xtrace 保持为通用的 AI/LLM observability substrate，Nebula 在此基础上完成自己的领域语义、运行时适配和专属诊断逻辑。**

## 1. 边界结论

### 1.1 xtrace 负责的能力

- 通用的 traces / observations / metrics 接收、存储、查询与展示
- 标准化的上下文传播契约
- 通用的 trace/span/event 关联原语
- 通用的 metadata 存储与检索
- 通用的流式观测原语与基础聚合能力

### 1.2 Nebula 负责的能力

- `gateway -> router -> node -> engine` 内部的 Trace Context 透传落地
- Python 引擎侧的 shim、批量回传、UDS 或队列等具体实现
- `scheduling.decision`、`node.reconcile`、`engine.swap` 等领域特定语义
- Nebula 自身的诊断规则、阈值、根因解释与专属 Dashboard
- 与具体引擎或运行时深度绑定的采样、聚合与回传策略

## 2. xtrace 值得推进的优化项

这些优化项属于通用底座能力，可以提升 Nebula 集成体验，但不应把 xtrace 演变成 Nebula 专属控制面。

### 2.1 标准化上下文传播契约

xtrace 应明确跨服务传播 trace context 的标准契约，优先兼容 W3C Trace Context 与 OTLP 生态，而不是引入某个业务系统专属的 header 或 metadata 格式。

建议明确：

- HTTP 场景使用哪些标准头
- RPC / internal metadata 如何映射
- 哪些 key 属于 xtrace 预留字段
- 哪些 key 应由业务侧通过 namespacing 扩展

### 2.2 SDK 的可选传播钩子

`xtrace-client` 可以提供通用、可选的传播 middleware / hook，帮助业务系统自动注入和提取 trace context。

这类能力应保持通用：

- 自动注入 trace context
- 自动提取上游 context
- 统一挂载 trace/span correlation 信息

是否启用、如何接入，由业务系统自行决定。

### 2.3 流式观测原语

xtrace 可以补充通用的 streaming lifecycle 原语，而不规定业务系统必须逐 token 落库。

建议以事件模型表达：

- `stream_start`
- `first_token`
- `stream_progress`
- `stream_end`
- `stream_error`

业务系统可以在这些原语之上自行决定按 token、按 chunk 或按时间窗口聚合。

### 2.4 Metadata 与 Metrics 命名约定

xtrace 应明确平台保留字段与推荐命名空间，避免不同集成方混用字段、产生歧义或未来冲突。

建议文档化：

- 平台保留字段
- 推荐的 metadata 前缀约定
- 推荐的 metric name / label 命名方式
- 高基数 labels 的风险与约束

### 2.5 高频写入保护

Nebula 的接入会放大对写入吞吐和 cardinality 的要求，但这类保护能力本身属于通用 observability backend 能力。

建议关注：

- 批量写入与 flush 策略
- 高频 event / metrics 的采样或限流建议
- label cardinality 控制
- retention、truncation 与查询保护策略

## 3. 这些优化是否会干扰其他服务

如果按上述边界推进，**不会构成对其他服务的结构性干扰**。原因如下：

- 它们优化的是通用 contract、SDK 能力和承载保护，而不是 Nebula 专属语义
- 它们不会要求其他服务理解 Nebula 的调度、节点、引擎生命周期概念
- 它们不要求在核心 schema 中新增 Nebula 专属表结构或顶层字段
- 它们不要求现有集成方改变现有业务语义，只是提供更清晰的接入规范

潜在风险主要来自错误的实现方式，而不是这些优化项本身。

会干扰其他服务的做法包括：

- 在核心接口里增加 Nebula 专属 API 或 schema
- 默认要求逐 token 高频落库
- 把 Nebula 领域语义提升为平台保留字段
- 让 SDK 的传播逻辑与某个业务系统强绑定、且无法关闭

因此，这些优化应坚持以下约束：

- 以标准协议优先，而不是私有协议优先
- 以可选能力优先，而不是强制能力优先
- 以命名约定优先，而不是核心 schema 固化优先
- 以通用写入保护优先，而不是针对单一集成方定制优先

## 4. 这些优化是否属于通用性需求

属于，而且优先级不低。

原因是这些需求并不依赖 Nebula 的业务语义，而是几乎所有 AI/LLM 系统在规模化接入 observability backend 时都会遇到的问题：

- 分布式上下文如何标准传播
- 流式响应如何统一建模
- metadata / metrics 如何避免命名冲突
- 高频写入和高基数场景如何保护后端

因此，xtrace 可以把这些项视为平台能力增强，而不是 Nebula 定制开发。

## 5. 明确不纳入 xtrace 核心的内容

以下内容不应因为 Nebula 接入而进入 xtrace 核心：

- Nebula 的路由、调度、放置与节点状态机逻辑
- 对 vLLM、SGLang 或其他具体引擎的专属 shim 实现
- Nebula 领域特定的诊断解释层和运营视图
- 只对某一业务系统有意义的专属表结构、专属 API 或默认行为

在边界明确的前提下，Nebula 可以独立迭代自己的 adapter、引擎埋点和诊断体系，而 xtrace 持续演进通用观测底座能力。