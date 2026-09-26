# 实验包与部署 prepare

`rutis-protocol::prepare` 读取版本目录，验证全部文件和依赖图，产生只读的
`PreparedDeployment`。这个格式属于 #59 的实验实现，尚未冻结为发布接口。
prepare 不启动 executable、不 import Node entry，也不构造 Rust factory 或插件。

```sh
cargo run -p rutis-protocol --bin rutis-protocol-prepare -- deployment.json
```

CLI 输出部署原字节摘要、组的 trust/capabilities 与 image/environment/code 摘要、
成员、精确路由、导出名、event 权限、缺失路由和拓扑信息；不输出配置值。缺失路由
留在计划里供原生 Pending 使用。拓扑信息不是
等待全组成员 Active 的屏障；实际 RuntimeReady 和成员发布协议仍待 runner 接入。

## 包目录

包清单名为 `protocol-plugin.json`，目录末级必须是清单的精确版本，例如
`packages/database/1.0.0/`。JSON 拒绝重复键和未知结构字段。版本为三个不带前导零的
十进制分量；摘要为 64 个小写十六进制字符。以下是字段示例；尖括号摘要必须替换成
对应文件的真实原字节 SHA-256 后才能使用。

```json
{
  "id": "example.database",
  "version": "1.0.0",
  "protocol_family": "rutis-cordis-objects",
  "protocol_version": "0.experimental",
  "runtime": {
    "kind": "rust-rutis",
    "framework_version": "0.3.0",
    "executable": "runner",
    "runner": null,
    "environment": ["Cargo.lock"],
    "capabilities": ["object.scope", "callback.borrow"]
  },
  "plugin": {"kind": "rust", "factory": "database"},
  "files": [
    {"path": "runner", "kind": "executable", "sha256": "<runner SHA-256>"},
    {"path": "Cargo.lock", "kind": "dependency", "sha256": "<lockfile SHA-256>"},
    {"path": "database.bundle.json", "kind": "bundle", "sha256": "<bundle SHA-256>"},
    {"path": "config.json", "kind": "config_schema", "sha256": "<config schema SHA-256>"}
  ],
  "config_schema": "config.json",
  "provides": {
    "database": {"interface": "Database", "version": "1.0.0", "bundle": "database.bundle.json"}
  },
  "requires": {},
  "events": {}
}
```

`files` 必须列出除根清单自身外的所有文件。额外文件、缺文件、错误角色或摘要、非
普通文件、逃出版本目录的 symlink、循环目录链接均拒绝。路径必须是规范的相对路径，
不接受绝对路径、`..`、`.`、空分段、反斜线、冒号或 NUL。内部文件链接仍按目标
原字节校验。可用角色是 `code`、`dependency`、`bundle`、`config_schema`、`executable`。

所有 `bundle` 和 `config_schema` 文件都进行完整准入，未被服务使用的声明文件也不
例外。bundle 原字节摘要、精确版本和接口名必须匹配；配置使用同一受限 JSON schema
校验。实际出现的 object、callback 和 event 类型会要求对应 capability，不能通过
省略 bundle 的 `required_capabilities` 隐藏能力需求。stream、持久 callback、同步
bail/emit 和 waterfall 等未启用扩展在 prepare 阶段拒绝。

`environment` 非空且必须精确覆盖所有 `dependency` 角色文件。环境摘要是
`digest(json::canonical({files: {path: {sha256, executable}, ...}, aliases: {alias: canonical_path, ...}}))`，
同时绑定路径、内容、可执行标志及内部链接关系。dependency 的内部链接目标也必须
是 dependency，避免共享环境暗中引用成员私有代码。
Node 包必须纳入完整运行依赖文件，单列一个 lockfile 不能替代实际依赖库存。

Node 清单改用 `kind: "node-cordis"` 和实际 Cordis 精确版本，并声明
`runtime.runner: "runner.mjs"`、`plugin: {"kind":"node", "entry":"plugin.mjs"}`。
runner/entry 均须在 `files` 中以 `code` 声明，扩展名只接受 `.js/.mjs/.cjs`；Node
可执行文件也随包声明并校验。当前 prepare 要求 Linux ELF executable；框架身份和
能力声明还需由实际 runner hello 验证，文件准入不能证明 Node image 的运行行为。

Rust image 必须含 `.rutis.protocol.catalog` ELF section。宿主只读取 section，不能
通过执行 runner 来探测 factory。清单的 framework、environment、capabilities，以及
所选 factory 的配置 schema SHA 和 provides/requires 契约必须与该元数据精确一致。
catalog 不要求与宿主共享 rustc/libstd/SDK 动态库 ABI。

## 部署

`packages` 的值是相对于部署 JSON 所在目录的版本目录，不得通过链接逃出该目录。
组的 `trust` 是部署者选定的共享信任域名称，不是操作系统沙箱。

```json
{
  "id": "example",
  "packages": {"db": "packages/database/1.0.0"},
  "groups": {"rust": {"kind":"rust-rutis", "trust":"application"}},
  "native_services": {},
  "instances": {
    "db-a": {"package":"db", "group":"rust", "config":{"label":"a"}, "routes":{}, "exports":["database"], "events":{}},
    "db-b": {"package":"db", "group":"rust", "config":{"label":"b"}, "routes":{}, "exports":["database"], "events":{}}
  }
}
```

每个实例保留独立冻结配置。共享组要求 kind、executable SHA、Node runner SHA、
framework、environment 和 capabilities 精确相同；不兼容成员或空组拒绝。
相同摘要且内容相同的 artifact 字节在计划中共用不可变存储。

`routes` 的 key 必须是该包声明的 requires 名；value 为
`{"kind":"instance", "instance":"db-a", "service":"database"}` 或
`{"kind":"native", "service":"host.database"}`。目标实例必须明确导出该服务。
`native_services` 声明宿主适配器的 `{interface, version, bundle_sha256}`，实际原生
适配器绑定由后续宿主 runtime 完成。每条边都检查这三个字段精确相等，不能用 semver
兼容代替原字节一致。完整 required 图的同组或跨组循环都拒绝。

漏配的 required route 保留为 missing，不合成提供者。宿主动态 `TypeKey` 只在本地
编码部署、provider 和服务名的完整元组，避免名称中 `/` 的拼接歧义；不序列化 Rust
`TypeId`。路由的 source 由宿主计算，不能直接信任作者提供的权限来源。

包的 events 声明 `{bundle, event, publish, subscribe}`；实例的 events 声明
`{scope, publish, subscribe}`。publish/subscribe 为 parallel/serial 列表，部署只能
缩减包允许的模式。scope 当前冻结为宿主逻辑名称；M4 的真实原生子树、列表和 ready
映射尚未完成，不能把该字符串准入当作运行期事件作用域证据。

## Rust 注册表与启动边界

Rust runner 用 `embed_runner_catalog!(include_bytes!(...))` 嵌入 metadata，并把返回的
`runner_catalog_bytes()` 同时交给 `StaticFactories::admit`。每个 `StaticFactory::native`
绑定配置类型、配置 schema 原字节、契约声明及延迟调用的 native `PluginFactory`
构造器。注册表拒绝缺失、额外、重复或契约不同的 factory，不调用构造器来完成校验。

宿主授权启动后，`mount_prepared` 核对冻结成员的 runner/catalog 和 factory，使用冻结
配置先做 schema 与原生类型解码，再调用构造器和原生 config 校验。注册时的作者元数据
在 permit 登记前取得；构造器或元数据 panic 成为可观察错误。实际 apply、依赖门控、
effect 与清理由 rutis native fiber 执行。失效后不能自动重入旧 activation。

这仍不是完整的 wire 生命周期：native mount 不发布 protocol service，runner 必须
另行暂存导出，等待 HostActive ACK 后发布。相同 catalog 也不能代替实际 native 服务
适配器、参数/结果 codec 和依赖绑定的验证。

## 冻结与后续启动

配置、manifest 原字节、artifact 字节、bundle 和路由在 prepare 后保持不变。
`verify_unchanged()` 会重读版本目录，拒绝 manifest、库存、路径、可执行标志或摘要
变化。包的 snapshot SHA 与组的 code SHA 同时绑定原 manifest 和全部文件的摘要、
canonical path、可执行标志；仅改配置值不会改变组 code SHA。

Linux `Snapshot::materialize(&plan)` 只使用已冻结字节建立私有 0700 目录，从不重读
原包。文件归一化为 0444/0555；保留执行标志和内部链接。相同 artifact 内容以
hardlink 复用。每组建立一份 dependency tree，各包的 dependency 链接指向该组的
canonical tree，保持实际 Cordis/SDK 模块身份。相同包快照的多个实例共享代码目录，
配置仍各自独立。链接不会指回原包；原包删除后，已准入快照仍可使用。

生产启动必须使用 `SnapshotGroup::argv()` 的快照路径，而不是
`PreparedGroup::argv()` 的原路径。group/member clone 持有快照租约；
`Snapshot::cleanup()` 拒绝删除仍被租用的树。supervisor 必须直到进程与后代回收和
native consumer cleanup 都完成后才释放租约。目录权限与只读文件不是同 uid 插件的
OS 沙箱，也不代替真实监督恢复。

TS SDK 的 `npm --prefix protocol/ts run build` 生成 `dist/src` 和 `dist/generated` 的
JavaScript/声明文件，并把 `.ts` 相对导入改为 `.js`。这些输出可进入包的 dependency
库存。当前包构建仍需部署者纳入完整依赖，不自动生成生产插件部署。

快照 conformance 测试从删除原目录后的冻结文件启动实际 Node executable、Cordis
4.0.1 与编译后的本仓库 SDK，确认两个不同代码包共享同一框架类、内部 alias 保持
身份，并分别装载和清理 native 实例。它是固定启动 fixture，未实现 private IPC
生命周期。生产多成员 runner、RuntimeReady/发布屏障、监督恢复与真实旧插件迁移仍
在开发，当前 CLI 的计划不能直接作为完成部署的证据。
