# 一方插件的 dylib SDK 设计(2026-09-24)

> 对应 [#45](https://github.com/arcships/rutis/issues/45),跟踪 [#52](https://github.com/arcships/rutis/issues/52)。
> 状态:设计稿 + Linux 原型验证。**不改变默认形态**:一方插件默认静态链接,
> 宿主默认单二进制;本设计只为“不重新发布宿主就换一方插件代码”的少数场景提供可选路径。

## 一、目标与非目标

**目标**

1. 一方(可信)插件可以单独编译、单独发布,由宿主在运行时加载,并通过 rutis 现有的生命周期换代码:
   旧代卸载 → 消费者驱逐 → 新代装配 → 消费者重载。
2. 插件与宿主共享任意 Rust 类型(服务、事件、`Ctx`、tokio),不做序列化。
3. 不兼容的插件在加载前被拒绝,并给出可读原因;不依赖“dlopen 成功”来判断兼容。

**非目标**

- 三方插件、不可信代码、崩溃隔离、强制停止:走协议插件(#46–#48)。
- 卸载旧代码(dlclose):初版不做,见 §九。
- 状态迁移(Erlang `code_change`):不做,换代 = 干净的卸载 + 重装配(与 [research-hot-reload](research-hot-reload-2026-08-17.md) 一致)。
- 不同 rustc 版本之间的兼容(abi_stable 路线):不做;SDK 变动过频时再单独评估。
- 不中断替换:换代存在卸载到装载的空窗,见 #51。

## 二、与既有决策的关系

- **单二进制分发**([design-dual-core](design-dual-core-2026-08-20.md)):保持为默认。dylib 能力是宿主的**可选构建变体**,
  只有该变体需要随附 `librutis_sdk.so` 和 `libstd-*.so`(§十)。
- **生命周期热重载与代码热更正交**([research-hot-reload](research-hot-reload-2026-08-17.md) §四.3):本设计即该文档预留的
  “正交组合点”——dylib 负责拿到新代码,rutis 负责安全换件。
- **已删除的 hotplug 实验**(`14f8c53`,cdylib + C ABI + JSON 字符串):有序列化开销却没有隔离,`dyn Plugin` 过不了边界。
  本设计走相反方向:Rust ABI + 共享 SDK dylib,边界上直接传 `Box<dyn PluginFactory>`。
- **TypeId 跨 dylib 风险**(research-hot-reload §四.4):SDK 单份 dylib 使共享类型只有一个定义,TypeId 一致(原型已验证),
  注册表键无需换稳定 id。前提是共享类型必须在 SDK 内(§四)。

## 三、总体结构

```
                 ┌──────────────────────── 进程 ────────────────────────┐
                 │  host(dylib 变体)                                     │
                 │     └─ rutis-dylib(加载器,宿主侧)                     │
                 │                                                        │
 DT_NEEDED ───▶  │  librutis_sdk.so  = rutis + tokio + 接口 crate + 分配器  │ ◀── 只加载一份
                 │  libstd-<hash>.so                                      │
                 │                                                        │
 dlopen ───────▶ │  libplugin_a.so (v3)   libplugin_a.so (v4)  libplugin_b.so │
 (RTLD_LOCAL)    │    └ 只依赖 SDK;私有依赖静态链入各自 .so                 │
                 └────────────────────────────────────────────────────────┘
```

| crate | 形态 | 内容 | 变动是否换 SDK 版本 |
| --- | --- | --- | --- |
| `rutis-sdk` | `crate-type = ["dylib"]` | 重导出 rutis、tokio、跨边界的接口 crate;`#[global_allocator]`;SDK 身份常量;`export_plugin!` 宏 | 是 |
| `rutis-dylib` | rlib,只被宿主依赖 | 清单解析、校验、内容寻址缓存、dlopen、`DylibFactory`、模块登记表 | 否(宿主侧) |
| 插件 crate | `crate-type = ["dylib"]` | 只直接依赖 `rutis-sdk` 与自身私有依赖 | — |

加载器不放进 SDK:它只在宿主使用,放进 SDK 会让加载器的每次修改都迫使所有插件重编。

## 四、SDK:兼容单位

### 4.1 放什么

只放**跨边界必须共享同一份**的东西:

1. rustc 版本(隐含)与 `std`(动态 `libstd-*.so`);
2. `rutis`(全局状态如 `InstanceId` 计数器、注册表类型);
3. `tokio`(运行时上下文是线程局部变量,必须单份,否则插件里 `tokio::spawn` 找不到宿主运行时);
4. 接口 crate:插件之间、插件与宿主之间交换的服务类型、事件类型;
5. 全局分配器(§4.3);
6. 插件配置的公共载体:`serde_json::Value`(§7.3)。

**不放**:插件私有依赖。它们静态链入各自的插件 .so,类型不得出现在边界上(不得作为服务键、事件、跨插件传递的值)。

### 4.2 共享依赖的规则

- 插件 crate 若直接依赖 SDK 已包含的 crate(如 `tokio = "1"`),只要与 SDK 解析到同一版本,rustc 会链接 SDK 中的那一份
  (原型的 host2 已验证:rutis/tokio 的符号全部来自 `librutis_sdk.so`)。
- 若解析出 semver 不兼容的第二个版本,就会出现第二份拷贝,其类型与 SDK 中的同名类型 TypeId 不同。
  CI 用 `cargo tree -d` 检查:SDK 包含的 crate 在插件依赖树中不得出现重复版本。
- 插件**私有**定义的服务类型只在该插件内可见。同一插件的两个版本各自定义的同名私有类型 TypeId 不同,因此
  换代码时对外提供的服务键必须是 SDK 接口类型,否则消费者在换代后找不到服务。

### 4.3 分配器(必须)

原型实测:宿主二进制里定义的 `#[global_allocator]` **不作用于**插件 dylib——插件分配 1 MiB 时宿主的计数分配器没有看到。
若宿主用 mimalloc 而插件用 System,插件分配、宿主释放就是未定义行为。

规则:**`#[global_allocator]` 只能在 SDK 中定义**;宿主和插件都禁止定义。原型实测放进 SDK 后,插件的分配经过它。
默认用 System;若要换 mimalloc/jemalloc,属于 SDK 变更。`export_plugin!` 可加一个编译期检查辅助,CI 另用
`nm` 检查插件 .so 不导出 `__rust_alloc` 的自有实现。

### 4.4 panic 策略

SDK、宿主、插件必须全部 `panic = "unwind"`(`abort` 与 `unwind` 混用无法链接或行为未定义)。panic 策略计入 SDK 身份。
原型实测:插件内 panic 可被宿主 `catch_unwind` 捕获;rutis 已在 apply/清理/监听器边界捕获 panic(#7),入口函数另由
`export_plugin!` 生成的包装捕获。

## 五、身份与兼容性校验

### 5.1 为什么不能依赖 dlopen

原型的两个反例:

| 实验 | 结果 |
| --- | --- |
| (a) SDK 内容变化但 crate 元数据哈希不变,插件按新 SDK 编译,加载进旧 SDK 的宿主 | `dlopen` **成功**;只有显式身份比对拦下 |
| (b) SDK 的 tokio features 不同(`+net`),插件按它编译,加载进原 SDK 的宿主 | `dlopen` **成功**,插件正常运行——这次侥幸,不能作为兼容证明 |

Rust 符号修饰中的 crate 哈希只能拦下一部分不匹配,无法覆盖“同名同哈希但布局已变”的情况。因此需要两层显式校验。

### 5.2 L1:声明身份(编译期常量 vs 运行期值)

SDK 的 `build.rs` 计算 `SDK_ID`:

```
sha256(
  sdk 版本号, rustc -vV 全文, 目标三元组,
  Cargo.lock 中 SDK 子依赖树(包名+版本+来源+checksum),
  启用的 features, profile 中影响 ABI 的项(panic、debug-assertions、overflow-checks;opt-level、debuginfo 不计),
  规范化后的 RUSTFLAGS(见下)
)
```

- `pub const SDK_ID: &str` 被插件和宿主在编译期**内联**进各自的产物;
- `#[inline(never)] pub fn loaded_sdk_id() -> &'static str` 在运行期返回**实际加载的** SDK 中的值。

两者不等即拒绝(宿主一侧见 §5.4,插件一侧见 §7.1)。原型实验 (a) 正是被这一层拦下。它便宜、无需额外文件,能拦下绝大多数配置错误。

**输入必须与机器无关。** `SDK_ID` 会被编译进 SDK,任何机器相关的输入都会同时改变 L1 和 L2 的结果,破坏可复现构建。
例如两台机器分别用 `--remap-path-prefix=/build/a=/target` 和 `--remap-path-prefix=/build/b=/target`,原始 RUSTFLAGS
不同,`SDK_ID` 就不同;路径重映射只改写输出中的源路径,改不了已经算出的哈希。因此 RUSTFLAGS 按**白名单**规范化:

- 读取 `CARGO_ENCODED_RUSTFLAGS`,只保留影响代码生成或类型布局的项:`-C target-cpu`、`-C target-feature`、
  `-C panic`、`-C debug-assertions`、`-C overflow-checks`、`--cfg`、`-Z` 系列;排序后参与哈希。
- 明确忽略:`--remap-path-prefix`、`-L`、`-l`、`-C link-arg(s)`、`-C linker`、`-C debuginfo`、`-C opt-level`、
  `-C incremental`、`-C codegen-units` 等只影响路径、链接或优化而不影响布局的项。
- 遇到不在两张表中的项,`build.rs` 直接报错,要求先归类,避免新参数悄悄进入或漏出身份。
- `build.rs` 为以上输入声明 `rerun-if-env-changed`。

### 5.3 L2:产物身份(二进制哈希)

评估文档要求“版本号必须对应一组不可变、经过验证的构建产物”。L2 落实这一点:

- 每个发布的 SDK 版本对应一个 `librutis_sdk.so` 的 sha256(`sdk_artifact`),记录在 SDK 发布清单中;
- **宿主**构建时把它所链接的 SDK 产物哈希编译进自己(§5.4);
- 插件打包工具在插件构建完成后,读取同一次构建产出的 `librutis_sdk.so` 计算 sha256,写入插件清单;
- 宿主启动时计算自己加载的 SDK 文件的 sha256(安装布局已知路径,或 `dladdr` 反查),与编译进宿主的值比对;
  加载插件时再与插件清单比对。

L2 要求**可复现构建**:插件在自己的 cargo 调用中会从源码重建 SDK,必须得到逐字节相同的产物。原型实测:

| 条件 | 两个 target 目录的 release SDK 哈希 |
| --- | --- |
| 不加路径重映射 | 不同 |
| `--remap-path-prefix=<target>=/target --remap-path-prefix=$HOME=/home` | **相同** |

这次原型的 `SDK_ID` 用的是固定的环境变量,没有包含 §5.2 的真实生成逻辑。跨机器可复现(不同源码路径、
CARGO_HOME、target 目录、用户名)尚未验证,见 §十一 V1;V1 必须用最终的 `build.rs` 生成 `SDK_ID`,并让各机器的
重映射参数不同,以确认规范化有效。路径重映射参数统一写进 `.cargo/config.toml` 与 CI,但**不计入** L1。

**L2 不可复现时的退路**:一方插件与 SDK 在同一次 CI 构建中产出(同一 cargo 调用,产物天然一致),插件按 SDK 版本批量发布。
这正是评估文档中“SDK 升级时自动重编全部一方插件”的做法;代价是“单独发布”退化为“按 SDK 批次发布”。

### 5.4 宿主绑定与启动检查顺序

只比较“运行时 SDK”和“插件构建时 SDK”是不够的:宿主自己也按某个 SDK 的布局编译。部署时若误换了同名、符号可解析
但布局不同的 SDK,配套的新插件会通过校验,宿主却仍按旧布局访问共享类型,产生未定义行为。原型复现了这一场景:

| 场景 | 结果 |
| --- | --- |
| 宿主按 SDK A 编译;部署了 SDK B 与按 B 编译的插件;宿主不做自检 | 启动、加载、换代码、shutdown **全部通过** |
| 同上,宿主启动时比对自身内联的 `SDK_ID` 与 `loaded_sdk_id()` | 启动即拒绝 |

原型第二行按 Rust ABI 直接调用 `loaded_sdk_id()`,只验证了自检的**有效性**;最终设计把宿主的首次身份查询换成
C ABI 引导接口(§7.2),原因见下方启动顺序第 1 步。

因此宿主、运行时 SDK、插件三者的身份必须闭合。宿主构建时绑定 SDK 身份:

- **L1**:宿主内联 `SDK_ID` 常量(与插件相同机制);
- **L2**:两阶段构建——先构建 `rutis-sdk` 并计算产物哈希,再以 `RUTIS_SDK_ARTIFACT_SHA256` 环境变量构建宿主,
  由 `env!` 编译进宿主。第二阶段复用第一阶段的 SDK 产物(同一 target 目录、同一参数),打包工具在构建结束后
  复核 SDK 文件哈希仍等于宿主内嵌的值。

**启动检查顺序**(dylib 变体,在创建 root、加载任何插件之前):

1. 经 **C ABI 引导接口** `rutis_sdk_boot_id(buf, cap) -> usize`(§7.2)读取运行时 SDK 的身份字节,与宿主内联的
   `SDK_ID` 比对——不等则拒绝启动。这是整个序列中唯一一次在确认兼容**之前**的跨界调用:此时第 2 步的产物哈希
   尚未核对,不能假设运行时 SDK 与宿主构建时 SDK 布局一致,而 Rust ABI 不保证稳定。因此引导接口用 `extern "C"`,
   签名只含整数与字节缓冲,不返回 `&str`、不触碰任何 SDK 类型——即使 SDK 布局不同,最坏情况也只是读到错误的
   字节而被这一步拒绝,不会产生未定义行为。
2. sha256(实际加载的 SDK 文件)== 宿主内嵌的 `RUTIS_SDK_ARTIFACT_SHA256`——不等则拒绝启动。
3. 之后每个插件:清单中的 `sdk.id`、`sdk.artifact_sha256` 等于宿主的值,插件 .so 内嵌的 `SDK_ID` 等于宿主的值
   ——加载前核对引导元数据(§7.1 第 2 步),加载后再交叉核对(§7.1 第 5 步)。

第 1、2 步通过后,“宿主构建时 = 运行时”成立,此后对 SDK 的 Rust ABI 调用(包括 `loaded_sdk_id()` 本身)才是安全的;
第 3 步保证“插件构建时 = 运行时”,三者闭合。

## 六、插件产物与清单

每个插件发布为一个目录(或归档):

```
greeter-4.2.0-x86_64-unknown-linux-gnu/
  plugin.toml
  libgreeter.so
```

```toml
[plugin]
id = "greeter"                 # 稳定标识,用于版本保留与诊断
version = "4.2.0"
library = "libgreeter.so"
library_sha256 = "…"

[sdk]
version = "0.4.0"              # 人读
id = "…"                       # L1,与 .so 内联常量相同
artifact_sha256 = "…"          # L2

[interfaces]                   # 所需接口版本(接口 crate 名 → semver 要求),只用于给出可读的拒绝原因
"rutis-iface-llm" = "^1.3"

[build]
target = "x86_64-unknown-linux-gnu"
rustc = "rustc 1.98.1 (48a229cea 2026-09-01)"
lock_sha256 = "…"             # 插件 Cargo.lock 全文
```

清单与 .so 内嵌元数据由 `export_plugin!` 和打包工具共同保证一致;加载时双重核对:加载前静态解析引导元数据
(§7.1 第 2 步),加载后再调 `rutis_plugin_meta()` 交叉核对(§7.1 第 5 步)。

## 七、加载流程

### 7.1 三阶段加载:校验 → 加载 → 构造(全部在 `PluginFactory::build` 之外)

`PluginFactory::build` 在 `update` 的 dry-run 与实际装载时各调用一次,约定为纯构造(`crates/rutis/src/plugin.rs:32`),
不能在其中加载库。加载是宿主的显式操作:`unsafe fn Loader::load(dir) -> Result<Arc<Module>, LoadError>`。

1. **读清单并校验**(不触碰 .so 的代码):目标三元组;`sdk.id`、`sdk.artifact_sha256` 等于宿主在 §5.4 启动检查中确认过的值;接口版本;同 id 已保留版本数(§九)。
2. **静态解析 .so 引导元数据**(只读文件 I/O,不加载、不执行任何代码):定位 `export_plugin!` 嵌入的引导 blob
   (§7.2),核对其中 SDK 身份等于宿主的值、插件 id 等于清单值;定位不到或核对失败即拒绝。清单是打包工具写的
   **声明**,这一步核对的是**二进制自身携带的值**——`dlopen` 在返回前就会执行 ELF 初始化代码
   (`.init`/`.init_array`),没有这一步,一个 `library_sha256` 与文件一致、但 `sdk.id` 错标为当前 SDK 的包,
   会在运行期核对(第 5 步)之前执行初始化代码,违背 §一 目标 3 的“加载前拒绝”。
3. **复制到内容寻址缓存**:`<cache>/<library_sha256>/lib<name>.so`,复制后重新计算哈希并比对。所有平台都这样做:
   Windows 避免文件锁,Linux 避免原地覆盖已映射的 .so 导致 SIGBUS;同一哈希只复制一次。
4. **加载库**:`dlopen(path, RTLD_NOW | RTLD_LOCAL)`。`RTLD_NOW` 让未解析符号在此刻失败而不是在调用时崩溃;
   `RTLD_LOCAL` 使同一插件的多个版本可以共存(原型实测 v1/v2 同名 crate 同时加载,互不串线)。此时第 2 步已
   确认二进制自述身份兼容,初始化代码的执行不再违背“加载前拒绝”。
5. **核对运行期元数据**:调用 `rutis_plugin_meta()`,比对 `sdk_id`(L1,对比 `loaded_sdk_id()`)、插件 id 与版本和
   清单及引导 blob 一致——加载后的交叉核对,防止引导 blob 被篡改或与运行期值不一致。
6. **构造工厂**:调用 `rutis_plugin_entry()` 得到 `Box<dyn PluginFactory<ConfigValue>>`,并在此时读取一次 `name()`、`injects()`,连同插件 id 一起记入 `Module`(供 §8.2 的不变量检查使用,之后不再回调)。

任一步失败返回 `LoadError`,带插件 id、版本、失败步骤和原因;第 4 步之后失败的库不卸载(§九),计入诊断。

### 7.2 插件侧导出与引导接口

```rust
rutis_sdk::export_plugin! {
    id: "greeter",
    factory: GreeterFactory,        // impl PluginFactory<ConfigValue>
}
```

宏展开为:

- `#[no_mangle] pub fn rutis_plugin_meta() -> rutis_sdk::PluginMeta`(Rust ABI;包含 `SDK_ID` 常量、id、`CARGO_PKG_VERSION`);
- `#[no_mangle] pub fn rutis_plugin_entry() -> Box<dyn PluginFactory<ConfigValue>>`,内部用 `catch_unwind` 包住构造;
- 编译期断言:`panic = "unwind"`;
- 一段**引导 blob**(纯数据,无初始化代码),供宿主在 `dlopen` 之前静态解析(§7.1 第 2 步):
  `#[used] #[link_section = ".note.rutis.meta"] static RUTIS_BOOT_META: [u8; N]`,内容为 magic、格式版本、
  `SDK_ID`、插件 id,各带长度前缀,余下填零。宿主在文件字节中搜索 magic 定位,不解析 ELF/PE/Mach-O 结构,
  三平台同一套代码;定位不到按不兼容拒绝。

SDK 侧另导出一个 **C ABI 引导函数**,供宿主在验证运行时 SDK 之前调用(§5.4 第 1 步):

- `#[no_mangle] pub extern "C" fn rutis_sdk_boot_id(buf: *mut u8, cap: usize) -> usize`:把 `SDK_ID` 的 UTF-8
  字节拷入 `buf`,返回长度;`cap` 不足时返回所需长度且不写入。函数体只有拷贝与长度比较,不触碰任何 SDK 类型。

其余导出(`rutis_plugin_meta()`、`rutis_plugin_entry()`、`loaded_sdk_id()`)都是 Rust ABI(不是 `extern "C"`):
宿主与插件保证同一 rustc、同一 SDK,传 trait object 是安全的——这正是 L1/L2 要保证的前提,也因此它们的首次
调用必须发生在引导校验通过之后。

### 7.3 配置类型

`PluginFactory<C>` 的 `C` 必须是宿主与插件都认识的类型。统一用 `ConfigValue = serde_json::Value`:

- 宿主从配置文件读取后原样交给插件,插件在 `validate_config` 中反序列化并校验;
- 插件配置结构变化不影响 SDK,不需要换 SDK 版本。

需要强类型配置的插件族,可以把配置类型放进接口 crate(即进入 SDK),代价是配置变动即 SDK 变动。

## 八、接入 rutis 生命周期

### 8.1 换代码 = `update`

rutis 的 `FiberView::update(config)` 要求 config 类型不变、工厂不变(`crates/rutis/src/fiber.rs:1279`)。
让 config 携带模块,即可复用 update 的全部语义(dry-run、预取消、重启、消费者驱逐与重载):

```rust
pub struct DylibConfig { module: Arc<Module>, value: ConfigValue }   // 字段私有,经 DylibConfig::new 构造

/// spawn 时从首个模块取定,终身不变(对应 rutis 的静态依赖声明,D32f)。
struct DylibFactory { id: String, name: String, injects: Vec<TypeKey> }

impl DylibFactory {
    /// 模块必须与 spawn 时的插件 id、name、injects 完全一致。纯比较,无副作用。
    fn check_module(&self, m: &Module) -> Result<(), CordisError> { /* 不一致 → CordisError::Validation */ }
}

impl PluginFactory<DylibConfig> for DylibFactory {
    fn validate_config(&self, c: &DylibConfig) -> Result<(), CordisError> {
        self.check_module(&c.module)?;
        c.module.factory.validate_config(&c.value)
    }
    fn build(&self, c: &DylibConfig) -> Result<Box<dyn Plugin>, CordisError> {
        self.check_module(&c.module)?;       // 纯构造:库早已加载,检查也是纯比较
        c.module.factory.build(&c.value)
    }
    // name / injects 返回 spawn 时取定的值
}
```

宿主侧 API:

```rust
let view = loader.spawn(&ctx, &module_v3, config)?;     // = ctx.plugin_with(DylibFactory, DylibConfig::new(..))
loader.swap(&view, &module_v4, config).await?;          // 便捷封装:提前给出可读错误,再调用 view.update(..)
```

原型已验证整条链路:consumer 依赖插件提供的 `Greeting`,`update` 把 v1 换成 v2 后,consumer 被自动驱逐并以新服务重载
(输出 `["hello v1 …", "hello v2 …"]`),插件内 `tokio::spawn` 正常。

### 8.2 不变量放在工厂里,而不是 `swap` 里

rutis 的依赖声明是静态的(D32f):`injects` 在 spawn 时注册一次,`update` 不会改变依赖注册表。如果换上去的模块声明了
不同的依赖,它会在旧的依赖门控下运行。

`FiberView::update` 是公开 API,调用方可以绕过 `loader.swap` 直接 `view.update(DylibConfig::new(other, ..))`。
因此插件 id、`name()`、`injects()` 的一致性检查必须放在 `DylibFactory` 内部,而不是只放在 `swap` 里:

- `update` 的 dry-run 依次调用 `validate_config` 和 `build`(`crates/rutis/src/fiber.rs:1299-1300`),两处都会拒绝;
- rutis 首次装载不调用 `validate_config`,但每次装载都调用 `build`,所以 `build` 中的检查覆盖所有路径;
- 检查所需的 id、name、injects 在加载时已记入 `Module`(§7.1 第 6 步),比较是纯操作,符合 `build` 的纯构造约定。

不一致时返回 `CordisError::Validation`,提示“依赖声明或插件身份变化需要 dispose 后重新 spawn”。`swap` 只是提前给出
同样的错误。

### 8.3 旧代残留

换代后旧版本的代码仍在内存中,旧代的后台任务若不观察取消信号,会继续运行、继续访问能力。这与 #12/#41 的问题相同,
dylib 场景只会更明显(旧代码甚至来自另一个版本)。**#41(按装载代号拒绝迟到注册)是本设计上线的前置条件。**

## 九、版本保留:初版不卸载

- `Module` 持有库句柄,**永不调用 dlclose**,即使最后一个 `Arc<Module>` 被释放。理由:外部持有的 `Arc`、trait object
  的 vtable、`Drop` 实现、已 spawn 的任务、线程局部析构器都可能仍引用旧库的代码,无法证明已全部释放。
- 登记表记录每个插件 id 已加载的版本、各自仍被多少 fiber 使用(`Arc::strong_count`)、加载时间与映射大小,进入诊断。
- **保留上限**:每个插件 id 最多保留 N 个版本(默认 4,可配置)。超过时 `load` 返回 `LoadError::RetentionExceeded`,
  提示需要重启进程回收。上限计的是已加载过的版本,不因旧版本不再使用而减少。
- 将来若要支持卸载,需要:`ctx.spawn` 登记插件的所有任务、证明任务已结束、外部引用归零,并在各平台验证 dlclose 语义。另行立项。

## 十、构建与发布

### 10.1 宿主的两个构建变体

| 变体 | 链接 | 产物 | 能否加载插件 |
| --- | --- | --- | --- |
| 默认 | 全部静态 | 单二进制 | 否 |
| `--features dylib-plugins` | rutis/tokio/std 动态 | 二进制 + `librutis_sdk.so` + `libstd-<hash>.so` | 是 |

采用 Bevy `dynamic_linking` 的写法:宿主依赖 `rutis` 不变,另加可选依赖 `rutis-sdk`,启用 feature 时 `use rutis_sdk as _;`。
原型 host2 实测:同一份源码,默认构建不依赖任何 Rust 动态库;启用 feature 后 rutis 符号全部解析到 `librutis_sdk.so`。
宿主代码中的 `use rutis::…` 无需改动。

发布布局:三个文件放在同一目录,宿主以 `-C link-args=-Wl,-rpath,$ORIGIN` 构建。`libstd-<hash>.so` 取自对应工具链的
`<sysroot>/lib/rustlib/<target>/lib/`。

### 10.2 CI 与发布

1. 仓库固定 `rust-toolchain.toml`,路径重映射参数写入 `.cargo/config.toml`。
2. SDK 发布:先构建 `rutis-sdk` 并计算产物哈希,再以 `RUTIS_SDK_ARTIFACT_SHA256` 构建宿主 dylib 变体(§5.4 两阶段构建),
   复核 SDK 文件哈希未变;产出 SDK 发布清单(`id`、`artifact_sha256`、包含的 crate 与版本)。
3. 插件发布:检出 SDK 对应的 tag,与 SDK 共用锁文件构建插件;打包工具校验本次构建产出的 `librutis_sdk.so`
   哈希等于 SDK 发布清单中的 `artifact_sha256`,不等即失败(这就是 L2 的“验证”)。
4. SDK 升级时,CI 以新 SDK 重编全部一方插件并批量发布。SDK 升级节奏放慢(例如按季度);只加不改的变化也会产生新 SDK 版本。
5. 接口 crate 启用 `cargo-semver-checks`(#44)。

## 十一、原型验证记录

原型位于会话临时目录,未提交;环境为 Linux x86_64、rustc 1.98.1、rutis 0.3.0(`7d7402d`)。

| 验证项 | 结果 |
| --- | --- |
| 宿主依赖 SDK dylib 后自动动态链接 `libstd` | 是,需要随附 `libstd-<hash>.so` |
| 宿主、SDK、插件三方看到的共享类型 TypeId | 一致 |
| 插件内 `tokio::spawn` 使用宿主运行时 | 正常 |
| rutis 依赖门控、`update` 换代码、消费者自动重载 | 正常 |
| 插件分配、宿主释放;插件类型的 Drop 在宿主调用 | 正常(Drop 执行 1 次) |
| 宿主 `#[global_allocator]` 覆盖插件 | **否**;放在 SDK 中则覆盖 |
| 插件 panic 被宿主 `catch_unwind` 捕获 | 是 |
| 同一插件 crate 的两个版本同时加载(`RTLD_LOCAL`) | 正常,互不串线 |
| SDK 内容变化、元数据哈希不变 | `dlopen` 成功,仅 L1 拦下 |
| SDK 依赖 features 变化 | `dlopen` 成功,运行未出错(不代表兼容) |
| SDK 可复现构建(同机不同 target 目录) | 加路径重映射后逐字节一致 |
| 宿主可选动态链接(Bevy 写法) | 可行 |
| 旧宿主(SDK A)+ 新 SDK(B)+ 按 B 编译的插件,宿主不自检 | 全部通过(即 §5.4 的漏洞) |
| 同上,宿主启动时比对内联 `SDK_ID` 与 `loaded_sdk_id()` | 启动即拒绝 |

**未验证(实施前必须完成)**

- V1 跨机器可复现构建(不同源码路径、CARGO_HOME、target 目录、用户名,且各机器的重映射参数不同),
  必须使用 §5.2 的真实 `SDK_ID` 生成逻辑;不成立则采用 §5.3 的退路。
- V2 macOS:`install_name` / `@rpath`、`RTLD_LOCAL` 下同名 crate 多版本共存、两级命名空间的影响。
- V3 Windows:Rust `dylib` 的导出符号数量上限(DLL 导出表 65535 项,大型 dylib 可能超限)、加载锁、`std` DLL 的分发。
  若不可行,Windows 只提供静态变体。
- V4 release 配置(LTO、`codegen-units`、`opt-level`)下的同样验证;SDK 不得启用跨 crate 的 LTO。
- V5 跨库 trait object 的长期运行:旧版本插件留下的任务在新版本运行期间继续执行、析构发生在旧库代码中。
- V6 线程局部变量与 `tracing` 等带全局分发器的 crate(若进入 SDK)在跨库下的行为。
- V7 引导接口与引导 blob(本轮修订新增,原型未验证):C ABI 引导函数在布局不一致的 SDK 上确实只返回错误字节
  而不崩溃;magic 字节搜索在 macOS/Windows 产物(含 debuginfo、去符号、strip 变体)上的定位可靠性。

## 十二、实施步骤

1. **前置**:#41(旧代迟到注册)合入。
2. `rutis-sdk` crate:重导出、分配器、`SDK_ID`(`build.rs`,含 RUSTFLAGS 白名单规范化)、C ABI 引导函数、`export_plugin!`(含引导 blob)、`ConfigValue`。
3. `rutis-dylib` crate:宿主启动检查(§5.4,首次身份查询走引导接口)、清单、引导元数据静态解析(§7.1 第 2 步)、内容寻址缓存、三阶段加载、`DylibFactory`(含模块不变量检查)、`spawn`/`swap`、模块登记表与诊断、保留上限。
4. 宿主 `dylib-plugins` 构建变体与发布布局。
5. 打包工具(`cargo xtask pack-plugin`):生成清单、计算哈希、执行 L2 校验、`cargo tree -d` 检查。
6. CI:SDK 发布流水线、插件批量重编、V1–V4 的自动化验证。
7. 文档:插件作者指南(禁止事项:自定义分配器、`panic = "abort"`、边界上使用私有类型、依赖 SDK crate 的其他版本)。

## 十三、验收标准(对应 #45)

- [ ] 宿主 + 一个 dylib 插件:跨库 TypeId、downcast、trait object、异步调用、析构均有自动化测试。
- [ ] L1/L2 任一不匹配时加载被拒绝,原因可读;测试覆盖 §5.1 的两个反例。
- [ ] 宿主启动检查(§5.4):“旧宿主 + 新 SDK + 新插件”在启动时被拒绝;SDK 文件哈希与宿主内嵌值不符时被拒绝。
- [ ] 引导边界:宿主的首次身份查询走 C ABI 引导接口,在运行时 SDK 的产物哈希核对通过之前不存在任何 Rust ABI
  跨界调用;同名可解析但布局不同的 SDK 部署在启动第 1 步被拒绝(不依赖 UB 侥幸)。
- [ ] 引导元数据:清单 `library_sha256` 与文件一致、但 `sdk.id` 与 .so 内嵌值(引导 blob)不符的包,在 `dlopen`
  之前被拒绝,不执行任何初始化代码;引导 blob 与 `rutis_plugin_meta()` 不一致在第 5 步被拒绝。
- [ ] `SDK_ID` 规范化:仅重映射参数不同的两次构建得到相同的 `SDK_ID` 与 SDK 产物哈希;未归类的 RUSTFLAGS 项使构建失败。
- [ ] `swap` 加载新版本后,旧实例按 rutis 生命周期卸载,消费者切换到新版本服务。
- [ ] 插件 id、name 或 injects 不同的模块:经 `swap` 和直接 `view.update(..)` 两条路径都被拒绝,fiber 保持原模块运行。
- [ ] 保留上限生效,诊断中可见各版本使用情况与内存占用。
- [ ] 默认构建仍为单二进制,不依赖任何 Rust 动态库。
- [ ] V1–V4 有结论并记录在本文档。
