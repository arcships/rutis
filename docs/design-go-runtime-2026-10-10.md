# Go 插件运行时（设计稿）

[English](design-go-runtime-2026-10-10.en.md)

状态：设计稿，未实现。日期：2026-10-10。
依据：[多语言插件：每种语言一个运行时插件](design-multilang-runtimes-2026-10-03.md)（下称"总体稿"，本文是它 §十一 的 M4，并修订其中关于 Go 的一处，见 §3.4）、[M1](design-multilang-m1-2026-10-04.md)、[M2：Python 运行时](design-multilang-m2-2026-10-04.md)、[实例服务](design-instance-services-2026-10-08.md)、[插件 API](guide/plugin-api.md)、[Bun 运行时](design-bun-runtime-2026-10-09.md)（#198，本文在通道、`mount` 回复、远程监听、名字冲突和测试上与它对齐）。
基准：`main` `fafc595`。

## 一、要解决什么

总体稿把 Go 定为第四种语言：基础设施类库、云厂商 SDK、网络工具，很多只有 Go 版，或者 Go 版最完整。

Go 和 Node、Python 有两处根本不同：

- **不能在运行时加载代码。** `plugin` 包要求宿主和插件用同一份工具链、同一份依赖构建，而且装上不能卸。插件只能编译进一个可执行文件里运行。
- **分发的东西不一样。** Node、Python 插件以源码包分发（npm、PyPI），宿主把它们装进**同一个环境**，一个进程按名字装载全部插件。Go 的产物是**编译好的二进制**：作者按平台发布，或者用户 `go install`。要把几个作者的 Go 插件合进一个进程，部署机器上就得有 Go 工具链、把它们一起重新编译，而且它们的依赖要能合成一份（Go 的最小版本选择只允许每个模块一个版本），作者之间互不知情，这一点保证不了。

所以 Go 不能照搬"一种语言一个运行时进程"。本文的模型是：**一个二进制一个运行时，宿主同时运行多个 Go 运行时**，并把"多个"当作常态来设计：怎样按插件名找到二进制、怎样在不启动进程的情况下知道它有哪些插件、怎样只在用到时才启动它（§三、§八）。

本文还定下：Go 插件长什么样（§四），值和方法怎样跨进程（§五），Go 运行时的并发模型和可重入（§六，回答总体稿 §九），运行时进程怎样实现现有契约（§七），rutis-host 和安装（§九），换代码（§十），以及仓库、测试、分阶段（§十一 – §十三）。

不在本文范围：Go 侧完整的 Cordis（Go 插件只做叶子，和 Python 一样）；Go 的 `plugin` 包；宿主在运行时临时编译 Go 代码。

## 二、结论先说

1. **一个二进制一个运行时，宿主同时跑多个。** 一个二进制里可以有一个或多个插件（通常是同一个作者、同一个项目的一组插件），由它的 `main.go` 调用 `rutis.Serve(...)` 决定。宿主配置里列出若干二进制（或一个目录），每个二进制是一个独立的运行时进程，运行时名由二进制名得出（`go-weather`）。
2. **行名只写插件名：`go:<插件名>`。** 一个 `GoResolver` 管理全部 Go 二进制，按插件名找到包含它的那个二进制；两个二进制都有同名插件时报歧义，可以写成 `go:<二进制名>/<插件名>` 指定。
3. **二进制自己说明它有什么，不必先跑起来。** `<二进制> --rutis-manifest` 打印清单（插件名、`inject`、`provides`、配置 Schema、版本、SDK 和插件 API 版本）后退出。解析行时读清单，所以行在运行时启动前就有完整的依赖声明；清单按二进制文件的大小和修改时间缓存。
4. **用到才启动，不用就停。** 某个二进制的插件被一行解析到时才启动它的运行时；它的行全部卸载、而且 loader 里已经没有条目指向它时，空闲一段时间后停下。列出十个二进制、只用两个，就只有两个进程。
5. **同一个二进制里的插件互相调用不走 IPC**；不同二进制之间、Go 和其他语言之间，经 rutis 按名字转发，和跨语言是同一条路径。
6. **Go 运行时可重入，而且是天然的。** 每个进来的调用在自己的 goroutine 上执行，同步调用只阻塞发出它的那个 goroutine。所以多个 Go 运行时之间、Go 和 Node、Python 之间互相同步调用都不会卡死。这回答了总体稿 §九：叶子运行时（Python、Go）都可重入，只有 Cordis 运行时不可重入。
7. **调用链和取消都放在 `context.Context` 里。** Go 没有 goroutine 局部变量，服务方法的第一个参数可以是 `context.Context`，SDK 把调用链和取消放进去；插件调用别的服务时把它传下去。
8. **插件写法和其他语言一样是叶子**：`Inject`、`Provides`、`Config`、`Apply(ctx, config)`；`ctx.Use`、`ctx.Provide`、`ctx.Effect`。插件 API 版本仍是 1。配置用 Go 结构体，JSON Schema 由 SDK 从类型生成。服务方法默认 `async`，用 `rutis.Sync(...)` 标出同步方法。使用服务时用"函数字段的结构体"绑定。
9. **换代码 = 换掉一个二进制 + 重启它这一个运行时**，其他 Go 运行时不受影响。`rutis-host dev` 替开发者重建并重启项目自己的二进制。宿主不编译别人的插件。
10. **不同 SDK 版本构建的二进制可以一起跑。** 它们之间只通过线协议交互；清单里写着插件 API 版本，宿主不支持时拒绝装载并说明原因。
11. **SDK 不依赖任何第三方模块**，放在本仓库 `go/rutis`，按 Go 的子目录模块规则打 tag 发布。

## 三、分发与运行时

### 3.1 模型

```text
rutis 宿主
 ├─ LocalRuntime::node    ── Node 进程（Cordis）        ── JS 行
 ├─ LocalRuntime::python  ── Python 进程（叶子 SDK）     ── py:<模块> 行
 ├─ GoRuntimes（GoResolver + 按需启动）
 │    ├─ plugins/go/weather   ── 运行时 go-weather   ── 插件 weather
 │    ├─ plugins/go/netkit    ── 运行时 go-netkit    ── 插件 ping、dns、traceroute
 │    └─ plugins/go/k8s       ── （没有行用到，不启动）
 └─ LoaderPlugin + 每个已启动的运行时一个 RuntimeRowsPlugin
```

每个 Go 运行时内部和 Python 运行时一样：行依赖 `RuntimeRows#<运行时名>` 和插件 `inject` 的每个名字（叶子运行时，rutis 门控全部名字）；插件 `provides` 的服务从行自己的 fiber 投到 `host_key(name)`；进程退出时，它的行全部停下，用到这些服务的其他行也停下。

和 Python 运行时不同的地方：

| | Python 运行时 | Go 运行时 |
| --- | --- | --- |
| 进程数 | 一个环境一个 | 一个二进制一个，用到才启动 |
| 能装哪些插件 | 环境里装了什么就能导入什么 | 构建时定死，清单里列出 |
| 行名 → 进程 | 前缀就是运行时名（`py:`） | `GoResolver` 按插件名查清单 |
| 不启动也能知道声明吗 | 不能，要第二段放行刷新 | 能，读清单 |
| 依赖冲突 | 同一环境里只能有一份，冲突时开第二个环境 | 每个二进制各自一份，没有冲突 |
| 换代码 | 重新导入模块 | 换二进制、重启这一个运行时 |

### 3.2 二进制从哪里来

对宿主来说只有一种东西：一个能 `--rutis-manifest` 的可执行文件。它可以来自：

1. **作者发布的预编译二进制**：作者在 GitHub Releases 等处按平台发布（`weather-darwin-arm64` 等）。部署方下载，放进 Go 插件目录。
2. **`go install`**：部署方机器上有 Go 工具链时，`GOBIN=plugins/go go install example.com/weather/cmd/weather@v1.2.0`。
3. **部署方自己组合**：写一个 `main.go`，导入几个作者的插件包，编成一个二进制，换取更少的进程和同进程调用。这要求这些插件的依赖能合成一份，由部署方负责。
4. **插件项目自己**：开发时 `rutis-host dev` 构建的二进制（§十）。

插件作者分发的是两样东西：一个 Go 包（导出 `Plugin`，给想组合的人用），和一个 `cmd/<名字>/main.go`（只 `Serve` 自己的插件，给直接用二进制的人用）。`rutis-host new --lang go` 的模板两样都有。

### 3.3 为什么不坚持一个进程

- 合进一个进程意味着部署机器必须有 Go 工具链、每装一个插件都要重新编译，依赖冲突时无解，而且 Go 插件作者习惯的分发方式（发布二进制）就用不上了；
- Go 进程启动快（几毫秒到几十毫秒）、空闲时内存小（几 MB 到十几 MB），多几个进程的代价比 Node、Python 低得多；
- 用到才启动（§8.3），没用到的二进制不占进程；
- 想要一个进程的部署方仍然可以自己组合（§3.2 第 3 种），模型不变：组合出来的也只是一个二进制。

### 3.4 对总体稿的修订

总体稿 §二.7 说"一组插件编译进一个可执行文件，作为一个运行时进程拉起"，§八说"进程数等于用到的运行时实例数：一种语言默认一个，Go 是一组一个"。本文保留"二进制就是运行时"，修订两点：

- Go 的"一组"由**二进制的作者或组合者**决定，不由宿主决定；宿主同时运行多个 Go 运行时是常态，不是"需要隔离时才多开"；
- Go 的行名不带运行时名（`go:<插件名>`），由 `GoResolver` 按清单路由到具体的运行时。

## 四、Go 侧写法

### 4.1 一个插件

```go
package weather

import (
	"context"

	"github.com/arcships/rutis/go/rutis"
)

// 配置：Schema 由 SDK 从这个类型生成（§4.4）。
type Config struct {
	City string `json:"city,omitempty" doc:"要查天气的城市"`
}

// 用到的服务：函数字段的结构体（§4.3）。
type LLM struct {
	Ask func(ctx context.Context, question string) (string, error)
}

// 提供的服务：普通的 Go 类型，导出的方法就是服务的方法。
type Weather struct {
	llm  LLM
	city string
}

func (w *Weather) Today(ctx context.Context) (string, error) {
	answer, err := w.llm.Ask(ctx, "weather in "+w.city) // 把 ctx 传下去（§6.2）
	if err != nil {
		return "", err
	}
	return answer + " in " + w.city, nil
}

func (w *Weather) Unit() string { return "celsius" }

var Plugin = rutis.Define(rutis.Plugin[Config]{
	Name:   "weather",
	Inject: []string{"llm"},
	Provides: rutis.Provides{
		"weather": rutis.MethodsOf[*Weather](rutis.Sync("Unit")),
	},
	Apply: func(ctx *rutis.Ctx, config Config) error {
		var llm LLM
		if err := ctx.Use("llm", &llm); err != nil {
			return err
		}
		city := config.City
		if city == "" {
			city = "Oslo"
		}
		ctx.Provide("weather", &Weather{llm: llm, city: city})
		ctx.Effect(func(context.Context) error { return nil }) // 清理，可以没有
		return nil
	},
})
```

- `rutis.Define` 返回一个不带类型参数的 `*rutis.Definition`，这样 `Serve` 能接收配置类型各不相同的插件；
- `Apply` 返回错误即装载失败，行进入失败状态，已经注册的清理照常运行；
- 不用 Python 那种"模块级变量"的写法：Go 里一个包导出一个 `Plugin` 变量就是全部约定。

### 4.2 一个二进制：`main.go`

插件作者为自己的插件（一个或几个）提供一个 `main`，这就是可以分发的二进制：

```go
// example.com/netkit/cmd/netkit/main.go
package main

import (
	"example.com/netkit/dns"
	"example.com/netkit/ping"

	"github.com/arcships/rutis/go/rutis"
)

func main() {
	rutis.Serve(ping.Plugin, dns.Plugin)
}
```

部署方想把几个作者的插件合进一个进程时，写法相同，只是导入的包来自不同的模块（§3.2）。

- `Serve` 读命令行参数：`--rutis-manifest` 时打印清单后退出（§8.1）；否则是 `<channel> <project>`，与 Python 运行时相同（§7.1），连上 rutis，一直运行到会话结束，然后退出进程；
- 两个插件同名时 `Serve` 直接退出并说明是哪两个包，不等到运行时才报；
- 不用 `init()` 自动登记：哪些插件在这个二进制里，看 `main.go` 就知道。

### 4.3 `ctx`

`*rutis.Ctx` 实现 `context.Context`：它在这一行卸载时被取消。插件自己起的 goroutine 应该用它，卸载时就会停下。

| 方法 | 作用 |
| --- | --- |
| `ctx.Use(name, &target)` | 把服务 `name` 绑定到 `target`。`target` 是函数字段的结构体（下面），或者 `*rutis.Service`（按名字动态调用）。`name` 必须在 `Inject` 里 |
| `ctx.Provide(name, value)` | 提供服务，直到卸载，或调用返回的函数。`value` 的类型要和 `Provides` 里声明的一致 |
| `ctx.Effect(cleanup)` | 卸载时运行 `cleanup(context.Context) error` |

**函数字段的结构体。** Go 没有动态代理，最接近"拿到一个对象、调用它的方法"的写法是：

```go
type LLM struct {
	Ask    func(ctx context.Context, question string) (string, error)
	Models func(ctx context.Context) ([]string, error) `rutis:"listModels"`
}
```

- 字段名按 §5.2 的规则对应到方法名（`Ask` → `ask`），`rutis:"…"` 标签可以改；
- 函数的第一个参数可以是 `context.Context`（建议都写），最后一个返回值必须是 `error`，最多再有一个结果；
- `Use` 时检查：服务声明的每个方法里，结构体用到的都在；结构体里有、服务没有的方法，`Use` 报错，而不是调用时才报；
- 提供者在**同一个进程**里时，字段直接接到提供者对象的方法上，调用不经过 rutis，也不复制参数；签名不完全一致时（例如参数是不同包里同形的结构体），在进程内按 §5.1 的规则转换一次，不走 IPC；
- 不想写结构体时用 `*rutis.Service`：`svc.Call(ctx, "ask", &answer, "weather in Oslo")`。

### 4.4 配置的 Schema

`Plugin[Config]` 的 `Config` 是结构体时，SDK 用反射生成 JSON Schema：

| Go | JSON Schema |
| --- | --- |
| `string`、`bool`、整数、浮点数 | `string`、`boolean`、`integer`、`number` |
| 切片、数组 | `array` + `items` |
| `map[string]T` | `object` + `additionalProperties` |
| 结构体 | `object` + `properties`；没有 `omitempty` 的字段进 `required` |
| `json:"-"` | 不出现 |
| 标签 `doc:"…"` | `description` |
| 实现了 `rutis.Schemer` 的类型 | 用它自己给的 Schema |

只覆盖配置里常见的形状。更复杂的，`Plugin.Schema` 可以直接给一份 Schema（这时不再从类型生成）。配置交给 `Apply` 之前由 `encoding/json` 解码，解码失败就是装载失败。

`Config` 用 `rutis.NoConfig`（空结构体）表示插件没有配置。

## 五、值和方法

### 5.1 值怎样跨进程

和[插件 API](guide/plugin-api.md)的表一致，只是要说清 Go 的类型怎样对上：

| Go 的值 | 怎样传递 |
| --- | --- |
| `nil`、布尔、数字、字符串 | 复制。整数超出 ±2^53−1 时报错（线协议的数字是 JSON 数字，和 Python 的 `MAX_SAFE` 相同） |
| 切片、数组、`map[string]T`、结构体 | 递归复制，字段名和省略规则照 `json` 标签；实现了 `json.Marshaler` 的类型按它自己的编码 |
| `error` | 复制名字和消息。名字是错误的类型名（`*fs.PathError` → `PathError`），实现了 `Name() string` 的错误用它给的名字 |
| 函数 | 按引用：另一端调用的是原来那个函数 |
| `rutis.Ref(v)` | 按引用：另一端拿到代理，调用 `v` 的导出方法 |
| `[]byte`、channel、`complex`、非字符串键的 map、循环引用 | 不能跨进程 |

Go 和 Python、JS 不同的一点：**结构体既可以是数据也可以是对象**，SDK 不能像 Python 那样按"有没有方法"来猜。所以结构体一律按数据复制，要按引用传对象时显式写 `rutis.Ref(v)`。服务对象本身（`ctx.Provide` 的值）总是按引用。

收到的值按目标类型解码：参数、返回值、结构体字段是函数类型时，收到的引用变成一个调用远端函数的 Go 函数；类型是 `*rutis.Future` 时，收到的异步结果可以 `Await(ctx)`；其余照 `encoding/json` 解码。第一版**不接收对象引用**（Python 也不接收），`peer` 握手里不声明 `objects`；以后可以把收到的对象引用绑定到函数字段的结构体上（§十四）。

### 5.2 方法名和方法形状

服务的方法是 `Provides` 里那个类型的**导出方法**，名字按规则转成线上的名字：首字母小写，开头连续的大写缩写整体小写（`Today` → `today`，`URLFor` → `urlFor`，`ID` → `id`）。`rutis.Rename("Today", "today_v2")` 可以改。两个方法转出同一个名字时 `Define` 直接报错。

方法签名：

```text
func (T) M([ctx context.Context,] 参数...) ([结果,] [error])
```

- 有 `context.Context` 参数时，SDK 把调用链和取消放进去（§六）；
- 参数从线上按位置解码到参数类型；多出的参数报错，缺少的参数取零值（JS 和 Python 的调用方可能省略尾部参数）；
- 返回非 nil 的 `error` 即抛出；`panic` 被接住，作为 `Panic` 错误抛出，带上栈。

**形状（同步还是异步）。** `rutis.MethodsOf[*Weather]()` 默认把每个方法标成 `async`，`rutis.Sync("Unit", ...)` 把列出的方法标成 `sync`。理由：

- 对 Go 自己来说两者没有区别，调用总是阻塞当前 goroutine，返回结果；
- 对其他语言来说区别很大：`sync` 方法在 Node 里会阻塞事件循环直到 Go 返回。Go 方法常做网络 I/O，默认 `async` 更安全；
- 不从签名猜（例如"带 `context.Context` 的是 async"）：同步方法也可能要回调别的服务，也需要 ctx 带着调用链。

`async` 方法被调用时，Go 侧照常在 goroutine 里执行，线上回一个异步结果引用，完成时结算；调用方取消时，方法的 `ctx` 被取消。

## 六、并发、调用链与可重入

### 6.1 一个调用一个 goroutine

会话层的结构：

- 一个读 goroutine，只负责读帧、解析、分发；
- `return` / `throw` 交给等待它的那个调用（每个发出的调用一个 channel）；
- `call` / `invoke` / `get` / `await` 以及控制操作，每个在**新的 goroutine** 上执行；
- 写帧在一把锁下进行；导出表、导入表、服务表各自加锁。

所以 Go 运行时里**任何时候都可以执行任何进来的调用**：一个 goroutine 在同步等待，不妨碍别的调用执行。这就是 Python 运行时用"等待期间执行所有进来的调用"换来的可重入，Go 不需要额外做什么。握手里声明 `reentrant-sync`。

代价也和 Python 相同，而且在 Go 里更普遍：**服务方法会被并发调用**，插件提供的对象必须能被多个 goroutine 同时使用；调用别的服务时不要持有锁（服务可能回调回来）。这和写 `net/http` 处理函数的要求一样，Go 作者熟悉。

### 6.2 调用链放在 `context.Context` 里

同步调用带着 `path`，对端用它判断一个反向调用是不是属于自己正在等的那条链（Node 只执行属于这条链的调用）。Python 从"当前正在执行哪个调用"取出 `path`；Go 没有 goroutine 局部变量，所以：

- 进来的调用执行时，SDK 把 `path + [这个调用的 id]` 和取消一起放进传给方法的 `context.Context`；
- Go 发出调用时（函数字段、`Service.Call`、远端函数），从传入的 `ctx` 取出调用链作为 `path`；
- `Apply` 和清理拿到的 ctx 也带着 `rows.load` / `rows.unload` 的调用链。

**必须把 ctx 传下去。** Go 侧不会因为丢了调用链而卡死（它总是可重入），但对端可能会：Node 同步调用 Go 的方法，这个方法用 `context.Background()` 去同步回调 Node 传进来的函数；回调不带调用链，Node 把它当成无关调用延后，而 Node 正在等 Go 返回，两边互相等。规则和 Go 里传超时、取消一样："拿到 ctx 就往下传"。SDK 能做的检查：

- 函数字段和 `Service.Call` 收到 `nil` ctx 时 panic，提示传入调用方的 ctx；
- 收到的 ctx 不带调用链、而当前进程正有进来的同步调用在执行时，在开发模式（`RUTIS_DEV=1`，`rutis-host dev` 设置）下打印一次警告，指出调用发生的位置。这只是提示：不带调用链的调用也可能是插件自己的后台 goroutine 发出的，是合法的。

### 6.3 取消

- 进来的 `async` 调用被调用方取消（`cancel` 帧）时，方法的 ctx 被取消；
- Go 发出的调用：被调用的方法是 `async` 时，`ctx` 被取消（或超时）就发 `cancel`，调用立即以 `ctx.Err()` 返回，迟到的结果丢弃；是 `sync` 时，协议没有取消同步调用的帧，Go 侧仍然立即返回 `ctx.Err()`，迟到的结果丢弃，对端照常执行完；
- 行卸载时，这一行的 `*rutis.Ctx` 被取消。

### 6.4 `SyncWaitCycle`

Go 没有事件循环，自己不会产生 `SyncWaitCycle`。从别处收到时，它是一个 `*rutis.RemoteError`，`Name` 为 `SyncWaitCycle`，`errors.Is(err, rutis.ErrSyncWaitCycle)` 成立。

### 6.5 与总体稿 §九 的关系

总体稿要求接入新语言前回答"它的运行时是否可重入"。Go 的回答是可重入，而且不需要特殊处理，所以选定的方向是：**所有叶子运行时都可重入，只有 Cordis 运行时不可重入**。剩下的唯一风险仍是两个 Cordis 运行时之间的交叉同步调用，与 Go 无关。是否在 Rust 侧检测"两个不可重入运行时之间的同步调用"，留在总体稿 §九，不在本文决定。

## 七、运行时进程

### 7.1 启动

`<二进制> <channel> <project>`，和 `python -m rutis` 相同（`<二进制> --rutis-manifest` 只打印清单，见 §8.1）：

| channel | 用在 | Go 的实现 |
| --- | --- | --- |
| `fd:3` | Unix 本地，继承的 socket（`Launcher::inherit_fd`） | `os.NewFile(3)` + `net.FileConn` |
| `unix:<path>` | 拨回 | `net.Dial("unix", ...)` |
| `tcp:127.0.0.1:<port>` | Windows 本地（`Handover::Loopback`） | 拨号后先发 `RUTIS_CHANNEL_TOKEN` 的值和换行，随即从环境变量里删掉它 |
| `listen:ws://…` / `listen:wss://…` | 远程运行时（G2） | 见 §7.5 |
| `ws://…`（主动拨出） | — | 启动时报错：Go 运行时只监听（远程插件设计 §4.4） |

按行分帧，单条消息有长度上限，与 [#173](https://github.com/arcships/rutis/issues/173) 一致，超过时关闭通道。

`project` 对 Go 运行时没有用处（代码已经在二进制里），照样接收，作为插件的工作目录参考（`rutis.Project()`）。

`Serve` 之外再给一个 `rutis.ServeArgs(args []string) error`，给想在同一个二进制里加自己的子命令的人用。

### 7.2 契约

实现 Python 运行时现在实现的那一份（`runner.py`），特性声明 `["rows.v2", "hosts", "leaf", "scopes"]`，问候时的能力为 `signals`、`reentrant-sync`：

| 控制操作 | Go 运行时做的事 |
| --- | --- |
| `mount` | 回复 `{ services: {}, features, implementation: { name: "rutis-go", version }, engine: { name: "go", version: runtime.Version() } }`。后两个字段与 Bun 运行时相同，`rutis-host check` 打印它们 |
| `rows.schema(entry)` | 在插件表里查 `entry`：`{ config, inject, provides, version }`。查不到时报错，列出已有的插件名 |
| `rows.load(key, entry, config, isolate, inject, exports)` | 建行和导出槽位，解码配置，执行 `Apply`。失败时卸载这一行再报错 |
| `rows.update(key, config)` | 卸载后用新配置装载（叶子插件没有 volatile 字段） |
| `rows.unload(key)` | 先撤销这一行提供的服务（rutis 先听到撤销），再取消行的 ctx，再按注册的相反顺序运行清理 |
| `hosts.provide(name, methods, label)` / `hosts.withdraw(id)` | 登记、撤销 rutis 服务的代理，按 `scopes` 规则带标签 |
| `release` / `get` | 释放导出的服务对象；读属性（Go 里读导出字段） |
| `dispose` | 卸载全部行，等进行中的调用结束 |

`isolate`、实例的标签、服务 id（名字 + NUL + 标签）、导出句柄的规则都照 Python 运行时，不另起一套。

服务槽位的通知（`service(id, handle, version)`）在 `ctx.Provide` 返回前发出，所以 rutis 一定先听到服务、再收到 `rows.load` 的回复。这是 Python 运行时已经保证的顺序，Go 用同一把写锁保证。

### 7.3 卸载不等于卸掉代码

Go 不能卸载代码。行卸载后，插件的代码仍在二进制里，只是不再有这一行的状态。这和 Python 的情况一样（Python 也不真正卸载模块），对行的语义没有影响。

插件自己起的 goroutine 如果不看 `ctx.Done()`，卸载后还会运行。SDK 没法停下它们，这一点写进指南。

### 7.4 panic 与退出

- 进来的调用、`Apply`、清理里的 panic 都被接住，变成错误；
- 插件自己起的 goroutine 里没接住的 panic 会让整个进程退出（Go 的规则，SDK 改不了；也与需求文档 §5 规则 8 一致）：这个运行时的行全部停下，rutis 报告进程退出的状态。需要隔离的插件编进另一个二进制。SDK 提供 `ctx.Go(func(context.Context) error)`：在 goroutine 里运行函数、接住 panic、写到 stderr，行卸载时取消它的 ctx 并等它返回；
- 会话结束（rutis 关闭通道或进程退出）时，卸载全部行（每个清理最多等 5 秒），然后 `os.Exit(0)`：不让残留的 goroutine 拖住进程，rutis 在等它退出。

### 7.5 远程（G2）

Go 二进制容易部署到别的机器上，所以远程运行时对 Go 很有用：`<二进制> listen:wss://0.0.0.0:7443/rutis --id office-go --peer main <project>`，由别处的 rutis 控制。实现和 Python 的 `serve` 相同：一次服务一个控制方，新的连接接替旧的，旧租约清理完才问候新会话；令牌、证书来自 `RUTIS_TOKEN`、`RUTIS_CERT`、`RUTIS_KEY`。会话格式是端点格式（协议 3，WebSocket 子协议 `rutis.3`）。

细节与 Python 的 `rutis[network]`、Bun 运行时 §4 相同：不带 TLS 时只允许回环地址；token 用防时序攻击的方式比较；拒绝时区分 404（路径不对）、401（没带凭据）、403（token 不对）、400（子协议不对）；单条消息上限 16 MiB，超过时以 1009 关闭；心跳 30 秒无响应断开，可用 `RUTIS_HEARTBEAT` 调整；被接管的旧连接以 4002 关闭；启动后在 stderr 打印 `rutis: listening on …`。

SDK 不依赖第三方模块，所以 WebSocket 服务端在 SDK 里用 `net/http` 实现（只要服务端、只要文本帧，Python 的 `websocket.py` 约 200 行）。第一版（G1）不做远程。


## 八、Rust 侧：多个 Go 运行时

### 8.1 清单

`<二进制> --rutis-manifest` 向标准输出打印一份 JSON，然后以 0 退出，不连接任何通道、不执行任何插件的 `Apply`：

```json
{
  "manifest": 1,
  "sdk": "0.9.0",
  "pluginApi": 1,
  "plugins": {
    "weather": {
      "config": { "type": "object", "properties": { "city": { "type": "string" } } },
      "inject": ["llm"],
      "provides": { "weather": { "today": "async", "unit": "sync" } },
      "version": "v1.2.0"
    }
  }
}
```

- 每个插件的部分和 `rows.schema` 的回复完全相同（§7.2），由同一段代码生成，所以清单和运行中的进程说的一定一致；
- 读清单要执行这个二进制：Go 包的 `init` 会运行，和把它当运行时启动是同样的信任。宿主只对配置里列出的二进制这样做；
- 执行有超时（5 秒）。不是本平台的二进制会在这里失败，错误写明"不是为 <os>/<arch> 构建的"；
- `pluginApi` 高于宿主支持的版本时，这个二进制的插件都解析失败，错误写明要升级什么；
- 清单按（路径、文件大小、修改时间）缓存，三者都没变就不再执行二进制。

### 8.2 `GoResolver`

放在 rutis-loader（feature `go`），实现 `Resolver`，管理全部本地 Go 二进制：

- **来源**：若干文件和若干目录。目录里的每个可执行文件都算（跳过以 `.` 开头的）。解析到不认识的插件名时重新扫描一次目录，所以往目录里放进新二进制后，下一次 reconcile 就能用到；
- **运行时名**：由文件名得出：去掉扩展名（Windows 的 `.exe`）、转小写、非 `[a-z0-9-]` 的字符换成 `-`，加前缀 `go-`（`netkit` → `go-netkit`）。它同时是 `Runtime#…`、`RuntimeRows#…` 的键和会话的端点 id。运行时名不能与其他运行时（`node`、`py`、`bun`、远程运行时）重名（Bun 设计 §5.1 的规则）。`binaries` 里列出的两个文件得出同一个名字，或者与其他运行时重名，是配置错误；目录扫描到的文件这样冲突时，跳过它并在诊断里报出来；
- **解析 `go:<插件名>`**：在所有清单里找这个插件。找不到，返回 `NotFound`（`Chain` 继续问下一个解析器）；在两个及以上二进制里，报歧义，列出这些二进制，提示改写成 `go:<二进制名>/<插件名>`；
- **解析结果**：与 `RuntimeResolver` 对叶子运行时的结果相同（行依赖 `RuntimeRows#<运行时名>` 和 `inject` 的全部名字，`provides` 投到 `host_key`，实例和 `isolate` 规则不变）。实现上把 `RuntimeResolver::resolve` 里"由声明构造 `Resolved`"的部分提出来，两者共用；
- **meta**：`{ source: "go", binary, runtime, version, sdk }`，`rutis-host check` 显示它们；
- **缓存与过期**：解析结果随清单一起按文件状态缓存。文件变了，下一次解析得到新清单；正在运行的旧进程由 §十 处理。

`GoResolver` 解析出一行时，告诉 `GoRuntimes` 这个运行时被用到了（§8.3）。

Go 行在运行时启动前就有完整的依赖声明，所以总体稿的第二段放行对 Go 不是必需的。为了让所有运行时的行依赖同一种服务，仍然保留 `RuntimeRows`：`RuntimeRowsPlugin` 现在只认 `RuntimeResolver`，改成接受一个小 trait（运行时名 + 取出需要刷新的行），`GoResolver` 为每个运行时实现它，需要刷新的行是"清单在进程启动后变了"的那些。

### 8.3 `GoRuntimes`：按需启动、空闲停止

也在 rutis-loader（它要用 `Loader`），挂在 loader 之后：

```rust
let go = GoResolver::new(GoBinaries::new().dir("plugins/go").file("bin/weather"))
    .with_catalog(&catalog);
let go = Arc::new(go);
chain = chain.with_shared(go.clone());
// ... 挂 loader ...
root.plugin(GoRuntimes::new(go, project).idle(Duration::from_secs(60)));
```

- 每个运行中的 Go 运行时是 `GoRuntimes` 下面的一个子 `Ctx`，里面挂 `LocalRuntime::go(binary, project).named(运行时名)` 和它的 `RuntimeRowsPlugin`。停下一个运行时就是释放这个子 `Ctx`，不影响其他运行时；
- **启动**：`GoResolver` 解析出一行时，如果这一行的运行时没有在运行，就启动它。行此时在等 `RuntimeRows#<运行时名>`，运行时起来后照常启动。冷启动的多个运行时并行启动；
- **停止**：一个运行时的最后一行卸载后，等 `idle` 时长；到时检查 `Loader::entries()` 里是否还有未禁用的条目解析到这个运行时，没有就停下。期间又有行解析到它，就取消这次停止；
- **崩溃**：进程意外退出时，和现在一样，它的行停下，运行时状态记下退出原因，**不自动重启**。之后有行再解析到它也不重启，直到应用调用 `GoRuntimes::restart(名字)`，或者二进制文件变了（换上了新构建，值得再试一次）；
- **全部启动**：`GoRuntimes::eager()` 在挂载时启动所有列出的二进制，给不想在第一次使用时等进程启动的部署；
- **状态**：`GoRuntimes::runtimes()` 列出每个二进制的运行时名、状态（未启动 / 启动中 / 运行中 / 已停 + 原因）、插件和版本。

### 8.4 rutis-bridge

| 改动 | 位置 |
| --- | --- |
| `Launcher::go(binary, project)`：程序是二进制本身，`cwd` 为 `project`，Unix 上 `inherit_fd` | `rutis-bridge/src/runtime/process.rs` |
| `LocalRuntime::go(binary, project)`，名字默认由文件名得出（§8.2） | `rutis-bridge/src/runtime/local.rs` |
| Cargo feature `go`（rutis-bridge、rutis-loader）；`interop` 包含它；CI 加一项"只开 `go`"的编译检查 | 两个 `Cargo.toml`、`ci.yml` |
| `RowSchema` 带上 `version`，写进行的 meta | Bun 设计 B1 也要做这件事，先落地的一方做 |
| 远程 node 运行时的 `Naming::Npm` 不接收带已知运行时前缀的名字：已知前缀加上 `go:` | `rutis-loader/src/runtime.rs` |

只想跑一个固定二进制的 Rust 应用，也可以不用 `GoResolver`，直接 `LocalRuntime::go` + `RuntimeResolver::modules`（行名 `<运行时名>:<插件名>`），和 Python 一样。

### 8.5 远程 Go 运行时

另一台机器上的 Go 运行时没有本地文件可读清单，按现有的远程运行时处理：每个是一个 `RuntimePlugin::remote(名字)`，经 `rutis-bridge/peer` 行连上，行名 `<名字>:<插件名>`，声明经 `rows.schema` 读取。一台远程机器上跑几个 Go 二进制，就是几个远程运行时。

## 九、rutis-host 与安装

`rutis.json`：

```json
{
  "runtimes": {
    "go": { "dir": "plugins/go", "binaries": ["bin/weather"], "start": "on-demand", "idle": 60 }
  },
  "rows": [
    { "id": "weather", "name": "go:weather", "config": { "city": "Oslo" } },
    { "id": "ping", "name": "go:ping" }
  ]
}
```

- `dir`、`binaries`：二进制从哪里找，至少给一个；
- `start`：`on-demand`（默认）或 `eager`；`idle`：空闲多少秒后停下；
- `remote` 的 `language` 加 `go`。

命令：

| 命令 | 作用 | 阶段 |
| --- | --- | --- |
| `rutis-host check` | 列出每个二进制：运行时名、平台是否匹配、SDK 和插件 API 版本、插件；列出歧义的插件名；每一行解析到哪个二进制 | G2 |
| `rutis-host dev` | 没有 `rutis.dev.json` 时，看到 `go.mod` 就按 Go 项目处理（现在 `project.rs` 一看到 `package.json` 就选 Node，Bun 设计也要改这里） | G2 |
| `rutis-host new <name> --lang go` | 模板：插件包、`cmd/<name>/main.go`、测试、`rutis.dev.json`、发布工作流（打 tag 时 `go test`、`rutis-host check`，用 GoReleaser 或 `go build` 矩阵按平台发布二进制） | G2 |
| `rutis-host go add <模块>@<版本>` | 用本机的 Go 工具链 `go install` 到 `dir`。没有工具链时报错并说明另外两种方式 | G2 |
| `rutis-host go add <URL>` | 下载作者发布的预编译二进制到 `dir`，校验发布里给的 SHA-256 | G3 |

macOS 上从网络下载的二进制带隔离属性，没有签名时系统会拒绝运行。`check` 和启动时检查这一点（复用 dylib 的隔离检查），报出明确的原因和处理办法，宿主不自己去掉隔离属性。

`rutis-host run` 只运行现成的二进制，不需要 Go 工具链。

## 十、换代码

| 场景 | 怎样生效 |
| --- | --- |
| 开发（`rutis-host dev`） | 监视项目里的 `.go`、`go.mod`、`go.sum`；有变化时把项目的 `cmd/<name>` 构建到缓存目录里的**新文件**（`<name>-<序号>`），成功后让 `GoResolver` 用新文件替换旧的、重启这一个运行时，旧文件随后删除。构建失败时打印编译错误，旧进程继续运行。其他 Go 运行时不受影响 |
| 部署 | 部署方把新二进制放到原来的位置，然后调用 `GoRuntimes::restart(名字)`（rutis-host 里重启宿主）。rutis 不监视二进制 |

每次构建到新文件名，是因为 Windows 不允许覆盖正在运行的可执行文件。

重启一个运行时的效果和这个进程退出一样：它的行全部停下，用到它们服务的行也停下；新进程起来后，行按新清单重新解析、按依赖重新启动。其他二进制的运行时一直在跑。

## 十一、仓库与发布

- SDK 放在 `go/rutis`，模块路径 `github.com/arcships/rutis/go/rutis`，和 `python/rutis`、`node/rutis` 并列。版本随发布列车，tag 为 `go/rutis/v0.9.0` 这样的形式（Go 对子目录模块的要求）；发布就是推 tag，由 Go 模块代理拉取；
- 只依赖标准库。Go 版本要求：当前的两个稳定版本（写在 `go.mod` 里）；
- 包结构：`rutis`（插件 API、`Serve`、清单）、`rutis/rutistest`（不需要宿主的测试工具）；会话层放在 `internal/peer`，不对外承诺；
- **兼容承诺**：宿主和二进制之间只有线协议、清单格式和插件 API 版本三样东西。同一个协议版本内，任何版本的 Go SDK 构建的二进制都能被任何版本的宿主运行，只要插件 API 不高于宿主支持的版本。这一点对 Go 比对 Node、Python 更重要：二进制发布出去就不会再随宿主升级；
- 指南 `docs/guide/go-plugin.md`（中英文），插件 API 文档加 Go 一列。

## 十二、测试

| 测试 | 位置 | 验证 |
| --- | --- | --- |
| 会话层单元测试 | `go/rutis/internal/peer`（`go test`） | 帧的编解码；引用计数和释放；同步调用里回调到达；取消；`path` 从 ctx 取出；并发调用 |
| 会话一致性 | `rutis-bridge/tests/runtime_conformance.rs`：加一个 Go 的 `conformance` 目标（对应 `conformance-session.mjs` / `conformance_session.py`） | Rust 侧同一套会话一致性测试（`session::testing::session`）在 Go 上通过 |
| 运行时契约 × 通道 | `rutis-bridge/tests/session_matrix.rs`：`Runtime` 加 `Go` | 运行时一致性测试（`runtime::testing::runtime`）在 `fd:3`、拨回的 socket 上通过；G2 加 WebSocket |
| 不需要宿主的插件测试 | `rutistest.Load(t, weather.Plugin, config, services)` | 和 `rutis.testing` 一样：只用 `Inject` 里的服务；提供的服务带着声明的方法；测试结束时清理都运行了；严格模式下值按 §5.1 编码再解码 |
| 清单 | `go/rutis` + `rutis-loader/tests/go_rows.rs` | 清单与运行中进程的 `rows.schema` 一致；文件不变时不再执行二进制；插件 API 过高、平台不匹配时给出明确错误 |
| 多个二进制 | `rutis-loader/tests/go_rows.rs` | 两个二进制：行按插件名找到各自的二进制；同名插件报歧义，`go:<二进制名>/<插件名>` 可用；两个二进制的插件互相同步、异步调用；一个进程崩溃只停它的行和它们的使用者，另一个照常 |
| 按需启动 | 同上 | 没有行用到的二进制不启动；最后一行移除、空闲时间过后进程退出；期间新加一行则不退出；崩溃后不自动重启，换了文件后再解析会启动 |
| 运行时一致性 | `crates/rutis-loader/tests/multilang.rs`，加 feature `go` | 同一组叶子插件再写一份 Go；三种语言同时冷启动，互相调用；提供者移除时只停它的使用者；`inject` 门控 |
| Python 运行时专项的 Go 版 | `rutis-bridge/tests` 下 `python_runtime.rs` 的 Go 版，加 Go 列：`cancellation.rs`、`error_shape.rs`、`rpc_callbacks.rs`、`process_exit.rs`、`live_objects.rs` | features 齐全；取消到达方法的 ctx；错误形状往返（Go 错误的类型名、`Panic`）；进程因 panic 退出时服务撤回；引用与 release。一致性夹具 `conformance-session`、`conformance-weather`、`conformance-greeter` 写 Go 版，`crash()` 以状态码 17 退出 |
| 远程租约（G2） | `leases.rs`、`remote_rows.rs` 加 Go 列 | 与 Bun 设计 §8 相同的租约场景 |
| 交叉同步调用 | 同上 | Node 同步调用 Go，Go 在方法里同步回调 Node 传来的函数（带 ctx）：不卡死；Go 与 Python、两个 Go 运行时之间互相同步调用：不卡死 |
| 实例 | `instance_runtimes.rs` | Go 行在实例里：标签隔离、每个实例自己的服务 |
| Windows | 现有 Windows CI | loopback 通道上的 Go 运行时；`dev` 换文件重启 |

测试用的 Go 插件放在 `crates/rutis-loader/tests/fixtures/go`，测试开始时构建成两个以上的二进制。

CI：`runtimes-go` job，在 Linux 和 macOS 上用 `actions/setup-go` 装上当前的两个稳定版本，运行 `go test ./...`、`cargo test -p rutis-bridge --features go,…`、`cargo test -p rutis-loader --features go,…`；单一 feature 编译检查 `cargo check --no-default-features --features go`；Windows 加进现有的 `runtimes-windows`。

E2E：S2（[#186](https://github.com/arcships/rutis/issues/186)）加 `new --lang go` 的开发循环；S3（[#187](https://github.com/arcships/rutis/issues/187)）加 Go 行参与跨语言组合与崩溃恢复；S9（[#193](https://github.com/arcships/rutis/issues/193)）加"下载的 Go 二进制在没有 Go 工具链的干净环境里运行"。

## 十三、分阶段

| 阶段 | 内容 | 验收 |
| --- | --- | --- |
| G1 | `go/rutis`：会话层、叶子 SDK、`Serve`（`fd` / `unix` / `tcp` 通道）、清单、`rutistest`。Rust 侧：`Launcher::go` / `LocalRuntime::go`、feature `go`、`GoResolver`（文件和目录、歧义、清单缓存）、`GoRuntimes`（先只做 `eager`）、`RuntimeRowsPlugin` 的 trait 化 | §十二 中除按需启动、远程以外的测试通过；多个二进制同时运行；Linux、macOS、Windows |
| G2 | 按需启动和空闲停止；rutis-host：`runtimes.go`、`dev` 的重建与重启、`check`、`new --lang go`、`go add <模块>@<版本>`；指南；远程运行时（`listen:wss://`） | 按需启动的测试通过；用 `rutis-host new --lang go` 建项目，`dev` 下改代码后只有这个运行时重启；`rutis-host` 控制另一台机器上的 Go 运行时 |
| G3（按需） | 下载预编译二进制并校验；接收对象引用（握手声明 `objects`）；帮部署方生成组合用的 `main.go` | 有真实需求时再定 |

G1 不依赖 rutis-host 的改动，可以先在 Rust 宿主里用。

## 十四、待定

- **清单的取法。** 本文执行二进制（`--rutis-manifest`）取清单，代价是会运行 Go 包的 `init`。备选是构建时把清单写进二进制里的一个固定标记之后，宿主直接从文件里读（和 dylib 的元数据段类似），不执行任何代码；但配置 Schema 由反射生成，要在构建时多一步（`go generate`）。先用执行的方式，若有人要求"列出插件时不执行代码"再加。
- **空闲多久停下**：默认 60 秒是猜的。也可以默认不停（只做按需启动），由部署方打开空闲停止。
- **`run` 是否也能构建**：本文不允许，和总体稿"宿主不临时编译"一致。
- **默认方法形状**：本文定为 `async`（§5.2）。
- **函数字段之外的类型化使用方式**，例如由提供方的 Go 接口生成绑定代码（`go generate`）。
- **接收对象引用**：放在 G3。
- **调用链丢失的检测**（§6.2）：只在开发模式下警告。
- **Python、Node 是否也要多实例按需启动**：它们按环境分发，没有同样的需要，本文不涉及。
