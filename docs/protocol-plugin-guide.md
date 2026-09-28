# Rust / Cordis 跨进程插件开发指南

> 当前文档描述已有实验 API，包含手工绑定和事件 helper。用户要求的原生插件使用体验正在按[修订设计](design-protocol-plugins-2026-09-25.md)补齐；这些接线步骤将收进库的适配层，本指南不能作为业务插件已零改动兼容的证据。

本指南面向使用 rutis 编写应用、或为该应用编写插件的开发者。目标是在自己的应用中接入协议库，让 Rust/rutis 与 Node/Cordis 插件取得并调用对方的服务对象。

当前接口属于本仓库的实验源码 API，实际进程示例在 Linux 上验证。服务对象、借用回调和显式接线的基础事件已经可运行；宿主启动仍需要装配代码，尚无一条命令自动接入任意插件的入口。先运行完整设置示例，再按需接入自己的应用。

## 1. 应用、插件与协议库的关系

Host 指开发者使用 rutis 编写的宿主应用。源码中的 `HostObjects`、`HostEvents`、`HostGraph` 是协议库提供的组件，不代表另一个必须独立启动的宿主产品。

```text
开发者的 Rust 应用（示例中由 Rust 测试程序扮演）
  |
  +-- 原生 rutis 插件
  |     +-- 通过 Ctx 取得生成的服务客户端
  |
  +-- rutis-protocol
        +-- 服务路由、对象授权、事件监听列表
        |
        | 私有 IPC
        |
      Node 子进程
        +-- 原生 Cordis 插件
        +-- 原生 SettingsProvider / SettingsScope
        +-- TS 协议 SDK
```

真实对象留在创建者进程，接收者得到受授权和生命周期约束的代理。传回创建者时仍指向原对象，跨进程方法通过异步调用执行。

| 开发角色 | 需要写的内容 |
| --- | --- |
| 应用开发者 | 选择 bundle、插件与端口，连接运行器，装配发布与关闭，按需要接线事件 |
| 服务提供者 | 用生成的 Service 接口适配真实对象，通过原生上下文提供服务 |
| 服务消费者 | 声明原生依赖，取得生成客户端，await 方法，登记自己创建的资源 |

对象编号、交付 token、引用账本和控制帧由 SDK 管理，业务插件不自己序列化或路由这些字段。

## 2. 运行完整示例与接入源码

在本仓库根目录使用 `rust-toolchain.toml` 指定的工具链和 Node/npm。CI 使用 Node 24；依赖由 npm lockfile 固定，其中 Cordis 为 4.0.1，设置服务为 0.1.1-rc.2。

```sh
npm --prefix protocol/ts ci --ignore-scripts
npm --prefix host ci --ignore-scripts
cargo test -p rutis-protocol --test settings_ipc
```

预期：`existing_settings_scope_keeps_native_behavior_across_private_ipc ... ok`。

该场景实际装载发行包的 SettingsProvider、Cordis 插件及 rutis 消费者，只以内存实现替换存储的 load/persist。它验证原生默认值、有效/无效写入、对象传回、回调重入、双向事件和正常卸载。

| 完整代码 | 阅读内容 |
| --- | --- |
| [Rust 应用与消费者](../crates/rutis-protocol/tests/settings_ipc.rs) | `scenario` 是应用装配入口；`Consumer` 是插件，`Visitor` 是借用回调，`Publisher` 是固定事件端点 |
| [Node 运行器与提供者](../host/tests/fixtures/protocol-settings-peer.ts) | Cordis 装载、设置服务、服务暂存、事件接入与关闭 |
| [原生服务适配](../host/src/protocol-settings.ts) | 真实对象包装、稳定身份、业务方法与 owner 传回 |
| [接口描述符](../protocol/fixtures/settings.bundle.json) | 两端共享的服务、对象、回调及事件类型 |

Node 文件由 Rust 应用启动并继承私有 fd 3，不能单独直接执行。`fixture/start` 等是此示例的测试控制入口；通用运行器的正式生命周期入口见[生命周期参考](../protocol/lifecycle.md)。

自己的 Rust 应用使用同一检出的 rutis 与 rutis-protocol。例如应用目录与本仓库目录并列：

```toml
[dependencies]
rutis = { path = "../rutis/crates/rutis" }
rutis-protocol = { path = "../rutis/crates/rutis-protocol" }
serde_json = "1"
```

当前源码跟随 main 的待发布 rutis 0.4.0，并使用本实现分支新增的适配 API。不能仅换成已发行的 `rutis = "0.3.0"` 并假定接口相同，详见[来源与维护责任](protocol-plugin-implementation.md#来源与维护责任)。TS 示例按仓库源码布局导入 SDK，编译入口为 `npm --prefix protocol/ts run build`，尚不提供稳定的独立安装接口。

## 3. 定义接口与生成绑定

先定义服务及其返回对象的业务接口，再选择哪些对象可以交付。设置 bundle 的关系是：

```text
Settings
  +-- open(namespace) --> SettingsSection
  +-- inspect(section) --> 当前数据

SettingsSection
  +-- namespace         只读快照属性
  +-- read()            异步读当前状态
  +-- write(patch)      异步调用原生 update
  +-- replace(section)  异步调用原生 replace
  +-- visit(callback)   调用期间的借用回调
```

| 描述符类型 | 用途 |
| --- | --- |
| value | 符合声明 schema 的 JSON 数据 |
| object / scope | 返回对象或由插件、显式子范围持有的对象 |
| object / borrow | 调用参数中的临时借用，例如传回 owner |
| callback / borrow | 一次调用及登记任务期间有效的回调 |
| record / list / optional | 上述类型的明确组合 |

从同一份原始 bundle 字节生成两端绑定：

```sh
cargo run -p rutis-protocol --bin rutis-protocol-bindgen -- \
  protocol/fixtures/settings.bundle.json \
  protocol/generated/settings.rs \
  protocol/ts/generated/settings.ts
```

输出包含 bundle 的 SHA-256。两端同时使用新 bundle 和新生成产物，即使只改空白也会改变摘要。不要手改生成文件或只升级一端。Rust 用 `include!` 引入生成文件；TS 输出默认相对导入 `../src` 的 SDK，调整布局时保留对应关系。

普通 JSON 中形似对象编号的字段仍是数据。对象和回调用生成接口交付；任意 JS 实例、反射、stream 和持久回调不会自动成为协议对象。详细类型约束见[协议参考](../protocol/README.md#json-与描述符)。

## 4. 提供服务：Node 业务与端口

以下片段按本仓库 `host/src` 的相对路径书写。应用已选择 bundle，并在 Node 根上下文装载自己的原生 settings 提供者；程序化装配步骤见第 6 节。

```ts
import type { Context } from '@deepseek-ai/cordis'
import { settingsNamespace } from '@deepseek-ai/dsh-settings'
import z from '@deepseek-ai/schemastery'
import { NativePorts, Bundles } from '../../protocol/ts/src/services.ts'
import {
  BUNDLE_SHA256, exportInterfaceSettings,
} from '../../protocol/ts/generated/settings.ts'
import { settingsObjects } from './protocol-settings.ts'

export function settingsPorts(bundles: Bundles) {
  const ports = new NativePorts()
  ports.provide('settings', 'protocolSettings', {
    interface: 'Settings', version: '1.0.0', bundle_sha256: BUNDLE_SHA256,
  }, bundles, exportInterfaceSettings)
  return ports
}

export const settingsPlugin = {
  inject: ['settings'],
  apply(ctx: Context) {
    const scope = ctx.settings.register(settingsNamespace('protocol-example'),
      z.object({ count: z.number().min(0).default(2) }))
    ctx.provide('protocolSettings',
      settingsObjects(new Map([['protocol-example', scope]])))
  },
}
```

三个名字含义不同：wire 服务名 `settings` 用于端口匹配；Node 原生导出名 `protocolSettings` 对应 ctx.provide；inject 中的 `settings` 是本进程已有的原生服务。

程序化 `NativePorts.mount` 的最后一个参数 `localInjects: ['settings']` 声明这个本地依赖。缺依赖时由 Cordis 原生门控等待，失去依赖后旧上下文封闭。实际 mount/bind/stage 顺序见完整示例的 `fixture/start` 分支。

`settingsObjects` 包装明确选择的真实 SettingsScope，缓存同一原生对象的包装。write/replace 调用原服务，schema 与持久化留在 owner。应用只导出允许的 namespace，不导出整个原生服务的反射能力。

Rust 提供者同样实现生成的 `Interface…Service` trait，`ctx.provide_as` 注册真实 `Arc<dyn …Service>`，再用 `NativePorts.provide` 和生成 export 函数声明端口。双向实例见[原生服务测试](../crates/rutis-protocol/tests/services.rs)。

## 5. 消费服务：Rust 插件

以下完整插件类型放在已导入生成绑定的模块中；`api` 对应 `protocol/generated/settings.rs`。应用把已接受的协议根绑定到该插件的原生依赖键。此片段还使用 `serde_json`。

```rust
use api::InterfaceSettingsClient;
use rutis::{BoxFuture, CordisError, Ctx, Effect, Plugin, TypeKey};
use serde_json::json;
use std::collections::BTreeMap;

fn protocol_error(error: rutis_protocol::error::ProtocolError) -> CordisError {
    CordisError::PluginFailed(Box::new(error))
}

pub struct CounterConsumer { dependencies: Vec<TypeKey> }
impl CounterConsumer {
    pub fn new() -> Self {
        Self { dependencies: vec![TypeKey::of::<InterfaceSettingsClient>()] }
    }
}
impl Plugin for CounterConsumer {
    fn name(&self) -> &str { "counter-consumer" }
    fn injects(&self) -> &[TypeKey] { &self.dependencies }
    fn apply<'a>(&'a self, ctx: &'a Ctx)
        -> BoxFuture<'a, Result<Effect, CordisError>>
    {
        Box::pin(async move {
            let settings = ctx.require_as::<InterfaceSettingsClient>(
                TypeKey::of::<InterfaceSettingsClient>())?;
            let section = settings.open("protocol-example".into())
                .await.map_err(protocol_error)?;
            section.write(BTreeMap::from([("count".into(), json!(7))]))
                .await.map_err(protocol_error)?;
            let state = section.read(()).await.map_err(protocol_error)?;
            let owner_state = settings.inspect(section.clone())
                .await.map_err(protocol_error)?;
            assert_eq!(state, owner_state);
            Ok(Effect::Done)
        })
    }
}
```

应用的端口声明使用相同 wire 名和依赖键：

```rust
ports.require::<InterfaceSettingsClient>(
    "settings", TypeKey::of::<InterfaceSettingsClient>(), &bundles,
)?;
```

Node 消费端先用 ports.require 声明 wire 名、原生服务名、契约及生成 bind 函数，再由插件声明对应 inject；从原生上下文取得客户端后 await 方法。完整双向代码见[session 示例](../crates/rutis-protocol/tests/session_ipc.rs)与[Node 端](../protocol/ts/tests/fixtures/session-peer.ts)。

## 6. 应用如何完成接线

程序化设置示例的装配顺序如下。这些步骤属于应用/运行器集成，不放进普通业务方法。

```text
准入同一 bundle，选择插件及端口
  -> 预留运行器/装载身份和同一 admission gate
  -> 建立私有连接，固定该连接的运行器身份
  -> 提供者原生 apply：绑定原始上下文，提供真实对象
  -> native ready，暂存完整导出表
  -> 应用确认发布，交付服务根，接收者确认 Accept
  -> 安装原生依赖客户端，装载消费者
  -> 绑定消费者原始上下文，完成其导出与发布
  -> 安装并确认事件端点，开始业务
```

| 步骤 | 实际 API |
| --- | --- |
| 准入与宿主组件 | Bundles::admit、HostObjects::new、HostEvents::new |
| 本端运行时 | RuntimeObjects::new、reserve；identity 由应用固定，业务参数不能声明调用者身份 |
| 私有连接 | Peer::start、host.handler(identity, fallback)、runtime.handler(fallback)、两端 attach；保留 Peer 的生命期 |
| 原始上下文 | 在真实 apply 的 Ctx/Context 上 Exports::managed / Exports.managed，随后 runtime.bind |
| 服务暂存 | Rust ports.stage / TS member.stage 复用已绑定的同一 Exports；stage_services / stageServices 安装调度器 |
| 服务交付 | host.offer_services 返回 RootDelivery；本地用 receive，跨连接用 send / 通用 driver 的 start，必须确认完整 Accept |
| Rust 原生注入 | ports.scope、ports.install、ManagedActivation::mount_gated；成功后 bindings.adopt，失败等待 rollback |
| TS 原生注入 | ports.mount；传完整 required 表、同一 gate，必要时显式 localInjects |
| 发布 | 原生就绪及相应确认后 host.publish / runtime.publish；通用生命周期路径还要求精确 activate ACK |

不要用 runtime root 代替真实业务 apply 的上下文绑定导出，也不要在 stage 时创建第二张表。示例自定义消费者先 bind 才运行具体业务；通用原生 driver 在业务 apply 前完成绑定。接收泵和 SDK 处理 object/*、授权、交付、释放及回调重入，应用不为每个业务方法写 JSON handler。stdout/stderr 留给输出和诊断，协议使用单独的私有连接。

通用 Rust NativeDriver::with_services / TS NativeModuleDriver 已装配对象和借用回调路径。静态 factory、Node 模块 protocolPorts 与启动契约见[生命周期参考](../protocol/lifecycle.md)。当前通用 Node driver 不传 localInjects，两端通用 driver 都拒绝事件 capability；上述含本地 settings 依赖和事件的 bundle 不能直接套进该入口。完整设置示例使用的是程序化接线。

## 7. 对象、回调和资源的使用规则

读取当前状态使用异步方法；namespace 等不变信息可读代理快照，Rust 是 `section.namespace()?`，TS 是 `section.namespace`。同步跨进程调用、instanceof、原型与任意反射需改成明确接口操作。

传回 owner 使用 `settings.inspect(section.clone())` / `settings.inspect(section)`。协议保留对象及原授权视图，第三方转交拒绝；不要把业务 ID 查找另一份同名对象当作原对象传回。

借用回调实现生成的 callback Service，调用前绑定实际创建者。以下 Rust 片段在协议 Result 的函数中使用，visitor 是 Arc<dyn BorrowCallback0Service>：

```rust
use rutis_protocol::sdk::ClientHandle;
section.client().caller().bind_native(ctx, api::exportBorrowCallback0(visitor.clone()))?;
let value = section.visit(visitor).await?;
```

TS 对应使用 SDK 的 `bindNative(caller, ctx, exportBorrowCallback0(visitor))` 再调用客户端。回调可以重入 section；实际代码见 Rust Visitor。

回调只在本次调用及登记任务存续期间有效。handler 要登记后续异步工作时使用传入的 `CallContext.spawn`，SDK 等这些任务结束后关闭 borrow。脱离调用的任务不延长借用期，不把此回调用作 watch 的持久监听。

scope 对象归插件或显式子范围，卸载会关闭。提前释放时 Rust 调用 `section.client().proxy().release()`，TS 使用 SDK `release(section)`；同范围同视图的别名一起失效。释放控制由 SDK 发送，程序化集成需要 runtime.flush 确认；Rust clone 和 JS 垃圾回收都不等于远端清理确认。

本地共享对象保持原生生命周期，释放协议引用不自动调用业务 close。独占资源由 owner 登记 disposer，在最后执行引用结束后清理一次。自建资源登记在实际插件上下文的 effect 中，旧上下文不重新接入新服务。

## 8. 事件接入

事件需要应用显式接线。一条 onProtocolEvent 端点对应 Host 列表的一条监听，不再向另一份本地列表广播。以下片段在真实 apply 中使用，路径按 host/src 布局：

```ts
import { onProtocolEvent } from './protocol-events.ts'

const listener = onProtocolEvent(ctx, async payload => {
  const current = await payload.section.read(null)
  return current.count === payload.value.count ? payload.value.result : undefined
})
ctx.provide('protocolEvents', listener)
```

应用将其声明为生成的 EventListener 端口，交付对象后用 HostEvents.subscribe 注册远端调用适配器；本地监听进入同一列表。Host 选择固定 EventKey 的 scope/bundle/event，通过 bind_publisher 绑定真实上下文、导出表和允许的事件。

Node 发布者取得应用授予的 EventPublisher 后使用设置示例的 helper：

```ts
import { emitProtocolEvent } from './protocol-events.ts'

const report = await emitProtocolEvent(ctx, publisher, 'parallel', section, value)
if (report.errors.length) throw new Error(JSON.stringify(report.errors))
```

section 是原生 InterfaceSettingsSectionService 适配器，publisher 是已接收客户端，value 是声明的 JSON 对象。这个 helper 是设置场景的类型适配；其他业务定义自己的 payload 与端点，复用 HostEvents 和 SDK。

| 行为 | 用法 |
| --- | --- |
| parallel | 并发等待有效监听并处理 errors，不把返回值当作 serial 短路 |
| serial | 按 Host 顺序 Continue/Return；Rust 看 returned: Option<Value>，TS 看明确的 returned 标记，不用 truthiness |
| once | Host 注册时设置 once，进入前原子认领，重入不重复 |
| ready | 本地 subscribe 返回即 ready；远端先完成对象 Accept，示例 publisher 安装也返回 ready 确认 |
| 注销 | unsubscribe 同步关闭准入，await 等在途执行；监听内部不等待自己结束 |

TS 监听返回 undefined 表示 Continue，false/0/null 均可 Return；它与 Cordis 本地 serial 的 false/null 处理有差别，适配器保留跨进程语义。纯本地 ctx.on/emit 不自动跨进程。

结果目前限于声明的 JSON 值，对象可进入载荷但不能转交第三方。emit/bail/waterfall、任意原生过滤器、持久业务回调和流不属于当前支持范围。完整接线和生命期行为见[事件参考](../protocol/events.md)。

## 9. 启动、停止与错误处理

启动要等待原生 ready、服务交付确认与发布；apply 完成不等于所有对端已经可调用。必要依赖缺失时保持 Pending，不重复执行业务 apply 来催促启动。

停止时先关闭准入，再等待插件实际清理、SDK 释放与已启动执行；结束对端生命周期后关闭连接，等待子进程退出。scenario 尾部给出完整正常顺序；应用同时为失败路径等待 rollback/stop，保留错误。

停止或断连后丢弃旧客户端、对象、缓存属性和 Ctx，它们不会重绑定同名新服务。丢弃调用或 stop 的等待 future 不表示远端执行或清理结束；登记任务仍追踪到实际结束。本轮不提供自动恢复或重新装载入口。

| 错误码 | 先检查什么 |
| --- | --- |
| InvalidParams | 值、结构、编码是否匹配描述符，是否把 JSON 当成对象 |
| InterfaceMismatch | bundle 原字节/摘要/接口/版本，以及 inject 与端口声明 |
| CapabilityDenied | namespace、scope、方法、第三方转交；stage 是否用了另一张表 |
| ScopeClosed / StaleObject | 插件、对象、借用或别名是否结束，不继续复用旧代理 |
| Unavailable | owner 是否发布、依赖和连接是否可用；断连不等于清理已确认 |
| UnsupportedCapability | 入口是否安装事件适配，类型和分发模式是否支持 |
| Business | 原服务校验、handler 异常或 panic，例如 schema 拒绝负 count |

Rust 显式将协议错误转成插件错误，TS 保留 ProtocolError 的 code。事件既可能 dispatch 失败，也可能返回 report.errors，两个位置都要处理；清理错误不能只记录后宣称成功。

## 10. 验证自己的接入

优先验证实际业务：读取状态、重复取对象、传回 owner、回调重入及卸载后旧引用失效。使用事件时再验证参数、等待、错误和注销；不以仅返回输入的 handler 代替原生插件适配。

```sh
npm --prefix protocol/ts run check
npm --prefix protocol/ts test
cargo test -p rutis-protocol --test settings_ipc
```

最终 workspace 回归需要本地套接字和可用临时空间。本机曾遇到 /tmp 配额，可将测试临时文件放在工作区：

```sh
mkdir -p target/protocol-test-tmp
TMPDIR="$PWD/target/protocol-test-tmp" cargo test --workspace
```

详细 API 见[服务绑定](../protocol/services.md)、[session](../protocol/session.md)、[生命周期](../protocol/lifecycle.md)和[创建者上下文](../protocol/native-context.md)。实现与验收证据见[工作记录](protocol-plugin-implementation.md)；一般 rutis 应用开发见[开发手册](development-handbook.md)。
