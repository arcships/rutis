# rutis

Cordis 核心范式的 Rust 惯用实现(自 [min-cordis](https://github.com/eric8810/min-cordis) 的 Rust 工作区独立成库)——插件化内核 + 配置热更新 + 动态事件 + LLM agent 框架 + dsh 宿主桥,一条 workspaces 六个 crate。

## 核心范式(五支柱)

1. **插件 = 装配单元**:一次 `apply`,提供服务 / 监听 / 清理
2. **fiber = 生命周期容器**:六态状态机 + 依赖门控 + 级联卸载 + 恰好一次清理
3. **服务 = 类型键注册表 + isolate 作用域**
4. **事件总线 = 四分发语义**(emit / parallel / serial / waterfall)
5. **依赖驱动重载**:provider 卸载 → 消费者驱逐并自动重载

## 支柱之上的两块生产语义

**配置热更新(D32)**——运行中改配置、自动重启生效,复用 fiber 状态机的恰好一次清理与消费者驱逐,零新事务逻辑:

```rust
// 工厂:每代从当前 config 构造实例;injects 静态声明(与 Plugin::injects 对称)
struct MyFactory;
impl PluginFactory<MyConfig> for MyFactory {
    fn build(&self, cfg: &MyConfig) -> Result<Box<dyn Plugin>, CordisError> { /* ... */ }
}

let view = ctx.plugin_with(MyFactory, cfg_v1);
(&view).await?;                       // 装载
view.update(cfg_v2).await?;           // dry-run 不过则现状不动;通过则卸载重载,消费者自动驱逐
```

**动态事件键(D33)**——运行时才知道名字的事件(宿主事件名等)走类型化事件 + 动态限定名,四分发与生命周期清理免费继承:

```rust
ctx.events().on_keyed::<HostEvent>(&ctx, "session/event", listener)?;
ctx.events().emit_keyed(&ctx, name, Arc::new(HostEvent { /* ... */ }));
```

两项设计对齐 cordis 语义:依赖声明静态固化(源码对照验证),96 个原版 spec 中 58 个语言无关不变量全自动化对拍(详见 [cordis-spec-parity](docs/cordis-spec-parity-2026-08-18.md) 与 [D32/D33 设计](docs/design-config-hot-update-and-dynamic-events-2026-09-21.md) 的决策表与审计记录)。

## Crates

| crate | 内容 |
|---|---|
| [`rutis`](crates/rutis) | 内核:Ctx / fiber / registry / event bus / effect / `PluginFactory` 配置热更新 / keyed 动态事件(141 项契约与对拍测试) |
| [`rutis-cordis`](crates/rutis-cordis) | 业务无关基座桥:协议机制(`Wire` 传输接缝 / 在飞表 / 取消 / 超时 / 孤儿计数)+ cordis 词汇(hello 能力集 / evt mode / wf kind / 装载仲裁)+ 通用服务分发(`svc/call` + 流式 `svc/part`)+ 宿主事件链路(`evt/emit` → `HostEvent` keyed 事件,`EventOrigin` 三字段透传),零 dsh 知识 |
| [`aimux-llm`](crates/aimux-llm) | 独立 llm 服务插件:rutis 插件形态(apply → 注册 `llm` 服务),aimux 原生 DTO/StreamPart 即中性协议 schema;工厂/keyed 缓存/listModels 缓存/回落全在此,零桥零 dsh 知识 |
| [`rutis-dsh`](crates/rutis-dsh) | 入口与组合根:起 rutis 运行时、装载 aimux-llm、把注册表中的服务经基座桥供给宿主进程,宿主事件转发进内核总线(`rutis-dsh up`);`LlmFace` 是纯形状胶水,零 dsh 知识 |
| [`rutis-agent`](crates/rutis-agent) | 最小 agent 框架:aimux `LanguageModel` 服务 + `ToolsPlugin` + `AgentDriverPlugin`(流式 `followup` + waterfall 中间件 + `agent/*` 事件广播)+ 内存 session(原子持久化)+ ratatui TUI;minimal mode 内置 `bash` + `replace_text` 工具 |
| [`rutis-cli`](crates/rutis-cli) | 命令行形态:最小 coding agent TUI(源码构建 `cargo build -p rutis-cli`;crates.io 的 `cargo install rutis-cli` 为旧版,不含 rutui TUI) |

agent crate 一句话:**一个 aimux [`LanguageModel`](https://crates.io/crates/aimux-core) 服务 + 一个 `ToolRegistry` 插件 + 一个实现 `Agent` 接口的 driver 插件 + 一个内存 session(连续 loop 的事实源)**。

## 依赖布局

agent / cli crate 经 crates.io 版本消费 [aimux](https://crates.io/crates/aimux-core)(LLM 统一访问层,`LanguageModel` / `CallOptions`,329 provider),**无需并列检出**。要 hack 本地 aimux,在工作区根加未提交的 `[patch]` 指向本地路径即可。

## 快速开始

```bash
cargo test                                    # 全量 267 项:内核契约+对拍 / 事件键 / 配置热更新 / 桥 e2e / agent 三层中的前两层
cargo run -p rutis-cli -- --scripted          # 无 key 离线演示
cargo run -p rutis-agent --example demo       # 真实后端两轮对话 + 依赖驱动驱逐(需 DEEPSEEK_API_KEY)
cargo run -p rutis-agent --example tui        # 交互式 TUI,流式逐字 / 工具可见 / Esc 取消
cargo run -p rutis-agent --example tui_scripted   # 离线脚本后端,无需 key
cargo test -p rutis-agent --test real_backend -- --ignored   # 真实端到端(不进 CI)
```

provider / model 可用 `AIMUX_PROVIDER` / `AIMUX_MODEL` 覆盖(如本地 `AIMUX_PROVIDER=ollama AIMUX_MODEL=qwen3:8b`)。

## 验证体系

- **内核**:141 项契约与对拍(fiber 状态机时序 / 恰好一次清理 LIFO/聚合不压平 / 依赖门控 / 级联卸载 / 依赖驱动重载 / 事件四分发),另加配置热更新 15 条与动态事件键 11 条契约测试;D32/D33 经三轮独立评审 + 设计复盘 + cordis 全量对照审计,全部差异显式化(设计文档 §八-§十二)
- **agent 三层**:单元(`ScriptedLlm` 实现真 `LanguageModel`)→ 集成(aimux `MockReplayModel` 录制回放;双门控、卸载驱逐重载、fiber 卸载取消)→ 真实端到端(`#[ignore]`,需 key 手动触发)
- **桥**:MemoryWire 进程内 e2e + loopback TCP 真 Node e2e

## 文档

**内核与范式**
- [design-rust-port.md](docs/design-rust-port.md) — 内核设计(D1-D31 决策表)
- [cordis-spec-parity-2026-08-18.md](docs/cordis-spec-parity-2026-08-18.md) — 与 cordis 原版 96 spec 对拍判定
- [design-config-hot-update-and-dynamic-events-2026-09-21.md](docs/design-config-hot-update-and-dynamic-events-2026-09-21.md) — D32 配置热更新 + D33 动态事件键(设计 / 实施 / 三轮评审 / D32f 复盘 / cordis 对照审计,§一-§十二)
- [review-rust-impl-2026-08-17.md](docs/review-rust-impl-2026-08-17.md) — 实现评审

**桥与宿主**
- [design-dual-core-2026-08-20.md](docs/design-dual-core-2026-08-20.md) — 双核架构与经验锈化路线(rutis Rust 脊柱 × dsh TS 功能面)
- [design-dsh-bridge-2026-08-21.md](docs/design-dsh-bridge-2026-08-21.md) — dsh 桥 v1 设计(TS 插件接入 + 编排 API)
- [decision-aimux-llm-plugin-2026-08-23.md](docs/decision-aimux-llm-plugin-2026-08-23.md) — aimux-llm 独立插件裁决

**agent**
- [design-min-agent-2026-08-18.md](docs/design-min-agent-2026-08-18.md) / [design-agent-verification-tui-2026-08-18.md](docs/design-agent-verification-tui-2026-08-18.md) — agent 框架与 TUI 设计
- [design-minimal-mode-2026-08-18.md](docs/design-minimal-mode-2026-08-18.md) — minimal mode(bash + replace_text)

## License

MIT(继承自 [Cordis](https://github.com/shigma/cordis) © Shigma)。
