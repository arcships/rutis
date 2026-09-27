# 现有设置服务的协议适配

从运行示例到编写自己的插件，见[跨进程插件开发指南](../docs/protocol-plugin-guide.md)。本文对照现有设置服务的原生调用与协议调用。

场景使用锁定的 `@deepseek-ai/dsh-settings` 0.1.1-rc.2 和 Cordis 4.0.1。Node 实际装载发行包的 `SettingsProvider`，Rust 实际装载 rutis 消费者插件，通过私有 IPC 注入生成客户端。测试只提供内存存储的 load/persist；namespace 注册、schema 默认值、验证、写入串行化和卸载均使用原服务实现。

这适配的是现有桥依赖的设置服务；不要求把旧桥全栈改写。设置业务对象的 write/replace 也不引入部署配置更新或升级编排。

## 必要改动

| 原生调用点 | 协议调用点及原因 |
| --- | --- |
| `ctx.settings.register(namespace, schema)` | 留在 owner 进程。Host 只导出明确选择的 namespace，schema 与持久化策略仍属原服务 |
| `scope.get()` | Rust `await section.read(())` / TS `await section.read(null)`；状态不能跨进程同步读取 |
| `scope.update(patch)` / `replace(section)` | `await section.write(patch)` / `replace(section)`；保留原生验证和默认值，失败不改变原状态 |
| 原生 SettingsScope 对象 | `settingsSection(namespace, scope)` 缓存一份显式接口适配器；重复 open、事件发送及传回 owner 保持同一对象身份 |
| namespace 属性 | 生成代理的只读快照；关闭后读取也拒绝，不把旧属性当作仍可用的服务 |
| 临时访问回调 | `section.visit(callback)` 使用 borrow 回调；回调可重入 read，保存到调用外后明确失效 |
| 跨进程事件监听 | 用 `onProtocolEvent(ctx, listener)` 导出一条监听，由 Host 统一注册；不自动转发原生 settings/updated 或本地广播 |
| 跨进程事件发布 | `await emitProtocolEvent(ctx, publisher, mode, section, value)`；publisher 是 Host 授予的固定事件端点，显式等待并处理 errors |
| 原生 `scope.watch()` | 不导出持久回调；本场景通过上述显式事件接口观察。纯本地 watch 仍按原服务工作 |
| 卸载与释放 | Namespace 仍由实际 Cordis effect 注销；协议导出/代理由 managed SDK 关闭，在途调用完成后收尾 |

`NativePorts.mount` 的 `localInjects` 声明仍在本进程的依赖，例如原生 `settings`。它与协议端口分别声明，继续使用 Cordis 的原生 Pending/Active 门控；缺本地依赖不进入 apply，失去依赖后旧上下文封闭。不增加全局代理槽。

接口见 `fixtures/settings.bundle.json`，两端生成产物见 `generated/settings.rs` 和 `ts/generated/settings.ts`。服务适配见 `host/src/protocol-settings.ts`，事件接入见 `host/src/protocol-events.ts`。

## 运行与证据

在仓库根目录安装锁定依赖后运行：

```sh
npm --prefix protocol/ts ci --ignore-scripts
npm --prefix host ci --ignore-scripts
cargo test -p rutis-protocol --test settings_ipc
```

测试 `existing_settings_scope_keeps_native_behavior_across_private_ipc` 启动真实 Node 子进程，只继承私有 fd 3。Rust 插件通过本地 Ctx 获取 Settings 客户端；Node 获取 Host 授予的 EventPublisher。对象与事件调用统一经过 broker。

同一条路径证明：重复 open 保持身份；count 从默认 2 写成 7，负值被原生 schema 拒绝；传回 owner 仍读取同一 scope；borrow 回调使用实际创建者上下文并重入 read；replace 恢复原生默认值；Rust 和 Node 均可发布 parallel/serial，对象载荷在接收端可继续调用并传回 owner，0/false/null 短路值保留；卸载后 namespace 注册数归零、effect 一次、旧对象和缓存属性失效。

双向一般业务服务注入及两端返回状态对象另由 `session_ipc.rs`、`native_runner_ipc.rs` 验证；本场景重点验证发行包现有服务的实际适配。[基础事件](events.md) 说明 ready、once、注销与当前能力边界。
