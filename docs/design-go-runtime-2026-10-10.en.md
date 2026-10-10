# The Go Plugin Runtime (Design Draft)

[中文](design-go-runtime-2026-10-10.md)

Status: design draft, not implemented. Date: 2026-10-10.
Based on [Multilingual Plugins: One Runtime Plugin per Language](design-multilang-runtimes-2026-10-03.en.md) ("the overall design"; this document is its §11 M4, and revises one point it makes about Go, see §3.4), [M1](design-multilang-m1-2026-10-04.en.md), [M2: the Python runtime](design-multilang-m2-2026-10-04.en.md), [instance services](design-instance-services-2026-10-08.en.md), the [plugin API](guide/plugin-api.en.md) and [the Bun runtime](design-bun-runtime-2026-10-09.en.md) (#198; this document follows it on channels, the `mount` reply, remote listening, name conflicts and tests).
Baseline: `main` `fafc595`.

## 1. The problem

The overall design names Go as the fourth language: infrastructure libraries, cloud vendor SDKs and network tools often exist only in Go, or are most complete there.

Go differs from Node and Python in two fundamental ways:

- **It cannot load code at run time.** The `plugin` package requires host and plugin to be built with the same toolchain and the same dependencies, and a loaded plugin cannot be unloaded. Plugins can only run compiled into an executable.
- **What gets distributed is different.** Node and Python plugins are distributed as source packages (npm, PyPI); a host installs them into **one environment**, and one process loads all of them by name. Go's product is a **compiled binary**: the author publishes it per platform, or the user runs `go install`. Putting several authors' Go plugins into one process would need a Go toolchain on the deployment machine, a rebuild of all of them together, and dependencies that merge into one set (Go's minimal version selection allows one version per module). Authors who do not know about each other cannot guarantee that.

So Go cannot simply follow "one runtime process per language". The model here is **one runtime per binary, with the host running several Go runtimes at once**, designed with "several" as the normal case: how a plugin name finds its binary, how to know what a binary contains without starting it, and how to start it only when it is used (§3, §8).

This document also settles what Go plugins look like (§4), how values and methods cross processes (§5), the Go runtime's concurrency model and reentrancy (§6, answering the overall design's §9), how the runtime process implements the existing contract (§7), rutis-host and installation (§9), new code (§10), and repository, tests and phases (§11 – §13).

Out of scope: a full Cordis in Go (Go plugins are leaves, as Python's are); Go's `plugin` package; the host compiling Go code at run time.

## 2. Conclusions first

1. **One runtime per binary; the host runs several.** A binary holds one or more plugins (usually one author's or one project's set), as decided by its `main.go` calling `rutis.Serve(...)`. The host configuration lists binaries (or a directory); each binary is a runtime process of its own, named after the binary (`go-weather`).
2. **Rows name only the plugin: `go:<plugin name>`.** A `GoResolver` manages all Go binaries and finds the one containing the plugin. When two binaries have a plugin of the same name it reports the ambiguity, and `go:<binary name>/<plugin name>` picks one.
3. **A binary says what it contains without being started.** `<binary> --rutis-manifest` prints a manifest (plugin names, `inject`, `provides`, configuration schemas, versions, SDK and plugin API versions) and exits. Rows resolve from the manifest, so they have their complete declarations before the runtime starts; manifests are cached by the binary's size and modification time.
4. **Started when used, stopped when not.** A binary's runtime starts when a row resolves to one of its plugins; once all its rows are unloaded and no loader entry refers to it any more, it stops after an idle period. List ten binaries and use two, and there are two processes.
5. **Plugins in the same binary call each other without IPC**; between binaries, and between Go and other languages, calls go through rutis by name, the same path as across languages.
6. **The Go runtime is reentrant, without effort.** Every incoming call runs on its own goroutine, and a synchronous call blocks only the goroutine that made it. So synchronous calls between Go runtimes, and between Go and Node or Python, cannot deadlock. This answers the overall design's §9: the leaf runtimes (Python, Go) are reentrant, only the Cordis runtime is not.
7. **The call chain and cancellation travel in `context.Context`.** Go has no goroutine-local state; a service method may take a `context.Context` first, the SDK puts the chain and cancellation in it, and a plugin passes it on when it calls other services.
8. **Plugins are leaves, written as in the other languages**: `Inject`, `Provides`, `Config`, `Apply(ctx, config)`; `ctx.Use`, `ctx.Provide`, `ctx.Effect`. The plugin API version stays 1. Configuration is a Go struct whose JSON Schema the SDK derives from the type. Service methods are `async` by default; `rutis.Sync(...)` marks synchronous ones. Services are used through a struct of function fields.
9. **New code = replace one binary + restart its runtime**, leaving other Go runtimes alone. `rutis-host dev` rebuilds and restarts the project's own binary for the developer. The host does not compile other people's plugins.
10. **Binaries built with different SDK versions run together.** They interact only over the wire protocol; the manifest states the plugin API version, and a host that does not support it refuses to load the plugins and says why.
11. **The SDK depends on no third-party module.** It lives in this repository at `go/rutis` and is released by tag, following Go's rules for modules in subdirectories.

## 3. Distribution and runtimes

### 3.1 The model

```text
rutis host
 ├─ LocalRuntime::node    ── Node process (Cordis)        ── JS rows
 ├─ LocalRuntime::python  ── Python process (leaf SDK)    ── py:<module> rows
 ├─ GoRuntimes (GoResolver + start on demand)
 │    ├─ plugins/go/weather   ── runtime go-weather   ── plugin weather
 │    ├─ plugins/go/netkit    ── runtime go-netkit    ── plugins ping, dns, traceroute
 │    └─ plugins/go/k8s       ── (no row uses it: not started)
 └─ LoaderPlugin + one RuntimeRowsPlugin per running runtime
```

Inside, each Go runtime is like the Python runtime: a row depends on `RuntimeRows#<runtime name>` and on every name its plugin injects (a leaf runtime, so rutis gates every name); the services the plugin provides are published from the row's own fiber at `host_key(name)`; when the process exits, its rows stop, and so do other rows using their services.

Where it differs from the Python runtime:

| | Python runtime | Go runtime |
| --- | --- | --- |
| Processes | one per environment | one per binary, started when used |
| Which plugins it can load | whatever is importable in the environment | fixed at build time, listed in the manifest |
| Row name → process | the prefix is the runtime name (`py:`) | `GoResolver` looks the plugin name up in the manifests |
| Declarations without starting it | no: the second release stage refreshes them | yes: from the manifest |
| Dependency conflicts | one copy per environment; a conflict needs a second environment | each binary has its own; none |
| New code | the module is imported again | replace the binary, restart that one runtime |

### 3.2 Where binaries come from

To the host there is one kind of thing: an executable that answers `--rutis-manifest`. It can come from:

1. **A prebuilt binary the author publishes**, per platform, on GitHub Releases or elsewhere (`weather-darwin-arm64` and so on). The deployer downloads it into the Go plugin directory.
2. **`go install`**, when the deployment machine has a Go toolchain: `GOBIN=plugins/go go install example.com/weather/cmd/weather@v1.2.0`.
3. **A composition the deployer builds**: a `main.go` importing several authors' plugin packages, compiled into one binary, for fewer processes and in-process calls. It requires those plugins' dependencies to merge, which is the deployer's responsibility.
4. **The plugin project itself**: the binary `rutis-host dev` builds during development (§10).

So a plugin author distributes two things: a Go package (exporting `Plugin`, for those who compose) and a `cmd/<name>/main.go` (serving only the author's plugins, for those who use the binary). The `rutis-host new --lang go` template has both.

### 3.3 Why not insist on one process

- One process means a Go toolchain on every deployment machine, a rebuild for every plugin installed, no way out of dependency conflicts, and no use of the way Go authors distribute (binaries);
- Go processes start fast (milliseconds to tens of milliseconds) and are small when idle (a few MB to a dozen or so), so a few more processes cost much less than they would for Node or Python;
- runtimes start when used (§8.3), so binaries nobody uses take no process;
- deployers who want one process can still compose (§3.2, item 3), and the model does not change: a composition is just another binary.

### 3.4 Revising the overall design

The overall design's §2.7 says "a group of plugins is compiled into one executable launched as one runtime process", and its §8 that "the process count equals the runtime instances used: one per language by default, one per group for Go". This document keeps "the binary is the runtime" and revises two points:

- for Go, the "group" is decided by **the binary's author or composer**, not by the host; running several Go runtimes at once is the normal case, not "more only when isolation is needed";
- Go row names do not carry a runtime name (`go:<plugin name>`); `GoResolver` routes them to a runtime through the manifests.

## 4. Writing Go plugins

### 4.1 A plugin

```go
package weather

import (
	"context"

	"github.com/arcships/rutis/go/rutis"
)

// The configuration: the SDK derives the schema from this type (§4.4).
type Config struct {
	City string `json:"city,omitempty" doc:"the city to report on"`
}

// A service it uses: a struct of function fields (§4.3).
type LLM struct {
	Ask func(ctx context.Context, question string) (string, error)
}

// The service it provides: a plain Go type; its exported methods are the
// service's methods.
type Weather struct {
	llm  LLM
	city string
}

func (w *Weather) Today(ctx context.Context) (string, error) {
	answer, err := w.llm.Ask(ctx, "weather in "+w.city) // pass ctx on (§6.2)
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
		ctx.Effect(func(context.Context) error { return nil }) // cleanup, optional
		return nil
	},
})
```

- `rutis.Define` returns a `*rutis.Definition` with no type parameter, so `Serve` can take plugins whose configuration types differ;
- an error from `Apply` fails the load: the row fails, and the cleanups registered so far run;
- there is no "module variables" form as in Python: a package exporting a `Plugin` variable is the whole convention.

### 4.2 A binary: `main.go`

A plugin author gives their plugins (one or several) a `main`, and that is the binary they distribute:

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

A deployer combining several authors' plugins into one process writes the same thing, importing packages from different modules (§3.2).

- `Serve` reads the command line: with `--rutis-manifest` it prints the manifest and exits (§8.1); otherwise it takes `<channel> <project>`, as the Python runtime does (§7.1), connects to rutis, runs until the session ends, then exits the process;
- two plugins with the same name make `Serve` exit at once, naming both packages, instead of failing later at run time;
- no automatic registration in `init()`: `main.go` shows which plugins are in the binary.

### 4.3 `ctx`

`*rutis.Ctx` implements `context.Context` and is cancelled when the row unloads. Goroutines a plugin starts should use it, so they stop at unload.

| Method | Effect |
| --- | --- |
| `ctx.Use(name, &target)` | Bind the service `name` to `target`: a struct of function fields (below), or a `*rutis.Service` (dynamic calls by name). `name` must be in `Inject` |
| `ctx.Provide(name, value)` | Provide a service until unload, or until the returned function is called. `value`'s type must match what `Provides` declares |
| `ctx.Effect(cleanup)` | Run `cleanup(context.Context) error` at unload |

**A struct of function fields.** Go has no dynamic proxies; the closest thing to "get an object and call its methods" is:

```go
type LLM struct {
	Ask    func(ctx context.Context, question string) (string, error)
	Models func(ctx context.Context) ([]string, error) `rutis:"listModels"`
}
```

- field names map to method names by the rule in §5.2 (`Ask` → `ask`); a `rutis:"…"` tag overrides it;
- a function may take a `context.Context` first (recommended everywhere); its last result must be `error`, with at most one result before it;
- `Use` checks the struct against the service's declared methods: a field naming a method the service does not have fails `Use`, not the first call;
- when the provider is **in the same process**, the fields call the provider object's methods directly: no rutis, no copying. When the signatures do not match exactly (say, a parameter is a struct of the same shape from another package), the arguments are converted once in-process by the rules of §5.1, still without IPC;
- without a struct, use `*rutis.Service`: `svc.Call(ctx, "ask", &answer, "weather in Oslo")`.

### 4.4 The configuration schema

When `Config` in `Plugin[Config]` is a struct, the SDK derives a JSON Schema by reflection:

| Go | JSON Schema |
| --- | --- |
| `string`, `bool`, integers, floats | `string`, `boolean`, `integer`, `number` |
| slices, arrays | `array` + `items` |
| `map[string]T` | `object` + `additionalProperties` |
| structs | `object` + `properties`; fields without `omitempty` are `required` |
| `json:"-"` | left out |
| tag `doc:"…"` | `description` |
| a type implementing `rutis.Schemer` | the schema it gives |

This covers the shapes configurations usually have. For anything else, `Plugin.Schema` takes a schema as is (then nothing is derived). `encoding/json` decodes the configuration before `Apply` sees it; a decoding error fails the load.

`rutis.NoConfig` (an empty struct) means the plugin takes no configuration.

## 5. Values and methods

### 5.1 How values cross processes

As in the [plugin API](guide/plugin-api.en.md) table, spelled out for Go types:

| Go value | How it crosses |
| --- | --- |
| `nil`, booleans, numbers, strings | copied. Integers beyond ±2^53−1 are an error (numbers on the wire are JSON numbers; Python's `MAX_SAFE` is the same limit) |
| slices, arrays, `map[string]T`, structs | copied recursively; field names and omission follow `json` tags; a type implementing `json.Marshaler` encodes itself |
| `error` | name and message copied. The name is the error's type name (`*fs.PathError` → `PathError`), or what `Name() string` returns when the error has it |
| functions | by reference: the other side calls the original function |
| `rutis.Ref(v)` | by reference: the other side gets a proxy calling `v`'s exported methods |
| `[]byte`, channels, `complex`, maps with non-string keys, cycles | cannot cross |

One way Go differs from Python and JS: **a struct can be data or an object**, and the SDK cannot guess from "does it have methods", as Python does. So structs are always copied as data, and an object crosses by reference only when written `rutis.Ref(v)`. A service object (the value given to `ctx.Provide`) always crosses by reference.

Received values decode into the target type: where a parameter, result or struct field has a function type, a received reference becomes a Go function calling the remote one; where the type is `*rutis.Future`, a received asynchronous result can be `Await(ctx)`ed; everything else decodes as `encoding/json` does. The first version **does not receive object references** (Python does not either), so the handshake does not declare `objects`; later, a received object reference could bind to a struct of function fields (§14).

### 5.2 Method names and shapes

A service's methods are the **exported methods** of the type in `Provides`, named on the wire by a rule: the first letter lowercased, and a leading run of capitals (an acronym) lowercased as a whole (`Today` → `today`, `URLFor` → `urlFor`, `ID` → `id`). `rutis.Rename("Today", "today_v2")` overrides it. Two methods mapping to the same name make `Define` fail.

Method signatures:

```text
func (T) M([ctx context.Context,] params...) ([result,] [error])
```

- with a `context.Context` parameter, the SDK puts the call chain and cancellation in it (§6);
- parameters decode from the wire by position into the parameter types; extra arguments are an error, missing ones are zero values (JS and Python callers may omit trailing arguments);
- a non-nil `error` result is thrown; a `panic` is recovered and thrown as a `Panic` error with its stack.

**Shape (synchronous or asynchronous).** `rutis.MethodsOf[*Weather]()` marks every method `async`; `rutis.Sync("Unit", ...)` marks the listed ones `sync`. Why:

- to Go itself the two are the same: a call always blocks the current goroutine and returns the result;
- to other languages they differ a lot: a `sync` method blocks Node's event loop until Go returns. Go methods often do network I/O, so `async` is the safer default;
- the shape is not guessed from the signature (say, "methods taking `context.Context` are async"): a synchronous method may call back into another service too, and needs ctx to carry the chain.

An `async` method runs on a goroutine like any other; the wire answers with an asynchronous result reference that settles when the method returns, and when the caller cancels, the method's `ctx` is cancelled.

## 6. Concurrency, the call chain, and reentrancy

### 6.1 One goroutine per call

The session layer:

- one reader goroutine reads, parses and dispatches frames, nothing else;
- `return` / `throw` go to the call waiting for them (a channel per outgoing call);
- `call` / `invoke` / `get` / `await` and control operations each run on a **new goroutine**;
- frames are written under one lock; the export, import and service tables each have their own.

So the Go runtime **can run any incoming call at any time**: a goroutine waiting synchronously does not keep other calls from running. This is the reentrancy the Python runtime gets by running every incoming call while it waits; Go gets it without doing anything. The handshake declares `reentrant-sync`.

The cost is the same as in Python, and more pervasive in Go: **service methods are called concurrently**, so the objects a plugin provides must be safe for use by several goroutines at once, and a plugin must not hold a lock across a call into another service (that service may call back). Go authors know this requirement from `net/http` handlers.

### 6.2 The call chain lives in `context.Context`

A synchronous call carries a `path`, and the far end uses it to tell whether a reverse call belongs to the chain it is waiting on (Node runs only calls belonging to that chain). Python reads the `path` from the call it is currently running; Go has no goroutine-local state, so:

- an incoming call runs with `path + [this call's id]`, and its cancellation, in the `context.Context` its method receives;
- an outgoing call from Go (a function field, `Service.Call`, a remote function) takes its `path` from the `ctx` it is given;
- the ctx `Apply` and cleanups receive carries the chain of `rows.load` / `rows.unload`.

**ctx must be passed on.** Go itself does not deadlock when a chain is lost (it is always reentrant), but the far end may: Node calls a Go method synchronously, and that method synchronously calls back a function Node passed in, using `context.Background()`. The callback carries no chain, so Node defers it as unrelated, while Node is waiting for Go to return: each waits for the other. The rule is the one Go has for deadlines and cancellation: "pass on the ctx you were given". What the SDK can check:

- function fields and `Service.Call` panic on a `nil` ctx, telling the author to pass the caller's ctx;
- in development mode (`RUTIS_DEV=1`, set by `rutis-host dev`), a call whose ctx carries no chain while an incoming synchronous call is running in the process prints one warning naming where the call was made. It is only a hint: a call without a chain may legitimately come from a plugin's own background goroutine.

### 6.3 Cancellation

- an incoming `async` call cancelled by its caller (a `cancel` frame) cancels the method's ctx;
- an outgoing call to an `async` method: when `ctx` is cancelled (or times out), Go sends `cancel`, the call returns `ctx.Err()` at once, and a late result is dropped. To a `sync` method: the protocol has no frame cancelling a synchronous call, so Go still returns `ctx.Err()` at once and drops the late result, and the far end runs to completion;
- when a row unloads, its `*rutis.Ctx` is cancelled.

### 6.4 `SyncWaitCycle`

Go has no event loop and never produces `SyncWaitCycle` itself. Received from elsewhere, it is a `*rutis.RemoteError` named `SyncWaitCycle`, and `errors.Is(err, rutis.ErrSyncWaitCycle)` holds.

### 6.5 The overall design's §9

The overall design asks, before a new language joins: is its runtime reentrant? For Go the answer is yes, with nothing special to do. So the direction is settled: **every leaf runtime is reentrant; only the Cordis runtime is not.** The one remaining risk is crossing synchronous calls between two Cordis runtimes, which does not involve Go. Whether Rust should detect synchronous calls between two non-reentrant runtimes stays in the overall design's §9; this document does not decide it.

## 7. The runtime process

### 7.1 Starting

`<binary> <channel> <project>`, as `python -m rutis` (`<binary> --rutis-manifest` only prints the manifest, see §8.1):

| channel | used for | in Go |
| --- | --- | --- |
| `fd:3` | Unix, local: an inherited socket (`Launcher::inherit_fd`) | `os.NewFile(3)` + `net.FileConn` |
| `unix:<path>` | dialing back | `net.Dial("unix", ...)` |
| `tcp:127.0.0.1:<port>` | Windows, local (`Handover::Loopback`) | after dialing, send the value of `RUTIS_CHANNEL_TOKEN` and a newline first, then remove it from the environment |
| `listen:ws://…` / `listen:wss://…` | a remote runtime (G2) | see §7.5 |
| `ws://…` (dialing out) | — | an error at startup: the Go runtime only listens (remote plugins design §4.4) |

Messages are framed by lines, with a length limit per message as in [#173](https://github.com/arcships/rutis/issues/173); a longer one closes the channel.

`project` is of no use to the Go runtime (the code is in the binary); it is accepted anyway and offered to plugins as a reference directory (`rutis.Project()`).

Besides `Serve`, `rutis.ServeArgs(args []string) error` serves those who want subcommands of their own in the same binary.

### 7.2 The contract

The one the Python runtime implements now (`runner.py`), declaring the features `["rows.v2", "hosts", "leaf", "scopes"]`, and the capabilities `signals` and `reentrant-sync` in its greeting:

| Control operation | What the Go runtime does |
| --- | --- |
| `mount` | replies `{ services: {}, features, implementation: { name: "rutis-go", version }, engine: { name: "go", version: runtime.Version() } }`. The last two fields are as in the Bun runtime; `rutis-host check` prints them |
| `rows.schema(entry)` | looks `entry` up in the plugin table: `{ config, inject, provides, version }`. When it is not there, an error listing the plugins there are |
| `rows.load(key, entry, config, isolate, inject, exports)` | creates the row and its export slots, decodes the configuration, runs `Apply`. On failure, unloads the row and reports the error |
| `rows.update(key, config)` | unloads and loads again with the new configuration (leaf plugins have no volatile fields) |
| `rows.unload(key)` | first withdraws what the row provided (rutis hears the withdrawals first), then cancels the row's ctx, then runs the cleanups in reverse order of registration |
| `hosts.provide(name, methods, label)` / `hosts.withdraw(id)` | registers / withdraws a proxy for a rutis service, labelled by the `scopes` rules |
| `release` / `get` | releases an exported service object; reads a property (an exported field in Go) |
| `dispose` | unloads every row and waits for calls in progress |

`isolate`, instance labels, service ids (the name, a NUL and the label) and export handles follow the Python runtime; there is no second set of rules.

A service slot's notification (`service(id, handle, version)`) goes out before `ctx.Provide` returns, so rutis always hears of the service before it gets the reply to `rows.load`. The Python runtime already guarantees this order; Go guarantees it with the same write lock.

### 7.3 Unloading does not remove code

Go cannot unload code. After a row unloads, its plugin's code is still in the binary; only the row's state is gone. Python is no different (it does not really unload modules either), and the row semantics do not change.

Goroutines a plugin started that ignore `ctx.Done()` keep running after unload. The SDK cannot stop them; the guide says so.

### 7.4 Panics and exit

- panics in incoming calls, `Apply` and cleanups are recovered and become errors;
- an unrecovered panic in a goroutine the plugin started exits the whole process (Go's rule; the SDK cannot change it; also the requirements document's §5 rule 8): all rows of this runtime stop, and rutis reports how the process exited. Plugins needing isolation go into another binary. The SDK offers `ctx.Go(func(context.Context) error)`: it runs the function on a goroutine, recovers a panic and writes it to stderr, and at unload cancels its ctx and waits for it to return;
- when the session ends (rutis closes the channel or exits), the runtime unloads every row (giving each cleanup at most 5 seconds) and then calls `os.Exit(0)`, so stray goroutines cannot keep the process alive while rutis waits for it to exit.

### 7.5 Remote (G2)

Go binaries are easy to deploy on other machines, so a remote runtime is especially useful for Go: `<binary> listen:wss://0.0.0.0:7443/rutis --id office-go --peer main <project>`, controlled by a rutis elsewhere. It works as Python's `serve` does: one controller at a time, a newer connection taking over, and a new session greeted only once the old lease is cleaned up; the token and certificates come from `RUTIS_TOKEN`, `RUTIS_CERT` and `RUTIS_KEY`. The session speaks the endpoint format (protocol 3, WebSocket subprotocol `rutis.3`).

The details are those of Python's `rutis[network]` and the Bun runtime's §4: without TLS only loopback addresses are allowed; the token is compared in constant time; refusals distinguish 404 (wrong path), 401 (no credential), 403 (wrong token) and 400 (wrong subprotocol); a message is at most 16 MiB, and a larger one closes with 1009; the heartbeat disconnects after 30 seconds without an answer, adjustable with `RUTIS_HEARTBEAT`; a connection taken over closes with 4002; once listening, it prints `rutis: listening on …` on stderr.

Since the SDK depends on no third-party module, its WebSocket server is written on `net/http` (server side only, text frames only; Python's `websocket.py` is about 200 lines). The first version (G1) has no remote.


## 8. The Rust side: several Go runtimes

### 8.1 The manifest

`<binary> --rutis-manifest` prints JSON on standard output and exits with 0, without connecting any channel or running any plugin's `Apply`:

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

- each plugin's entry is exactly the reply to `rows.schema` (§7.2), produced by the same code, so the manifest and the running process always agree;
- reading the manifest executes the binary: Go packages' `init` functions run, the same trust as starting it as a runtime. The host does this only for binaries its configuration lists;
- execution has a timeout (5 seconds). A binary for another platform fails here, with an error saying it was "not built for <os>/<arch>";
- a `pluginApi` above what the host supports fails every plugin of the binary, with an error saying what to upgrade;
- manifests are cached by (path, file size, modification time); while none of them changes, the binary is not executed again.

### 8.2 `GoResolver`

In rutis-loader (feature `go`), implementing `Resolver`, managing all local Go binaries:

- **Sources**: files and directories. Every executable file in a directory counts (except names starting with `.`). A plugin name it does not know makes it scan its directories again, once, so a binary dropped into the directory is usable at the next reconcile;
- **Runtime names** come from file names: the extension removed (`.exe` on Windows), lowercased, characters outside `[a-z0-9-]` replaced by `-`, prefixed with `go-` (`netkit` → `go-netkit`). The name keys `Runtime#…` and `RuntimeRows#…` and is the session's endpoint id. A runtime name may not repeat another runtime's (`node`, `py`, `bun`, remote runtimes), the rule of the Bun design's §5.1. Two files listed in `binaries` giving the same name, or a name another runtime has, is a configuration error; a file found by scanning a directory that conflicts this way is skipped and reported in the diagnostics;
- **Resolving `go:<plugin name>`** looks the plugin up in every manifest. Not found: `NotFound` (the `Chain` asks the next resolver). Found in two or more binaries: an ambiguity error listing the binaries and suggesting `go:<binary name>/<plugin name>`;
- **The resolution** is what `RuntimeResolver` produces for a leaf runtime (the row depends on `RuntimeRows#<runtime name>` and every injected name, `provides` are published at `host_key`, instance and `isolate` rules unchanged). In code, the part of `RuntimeResolver::resolve` that builds `Resolved` from declarations moves out and both use it;
- **meta**: `{ source: "go", binary, runtime, version, sdk }`, shown by `rutis-host check`;
- **Caching and staleness**: resolutions are cached with the manifest, by file state. When the file changes, the next resolution gets the new manifest; the old process still running is §10's business.

When `GoResolver` resolves a row, it tells `GoRuntimes` that the row's runtime is in use (§8.3).

Go rows have their complete declarations before their runtime starts, so the overall design's second release stage is not needed for Go. `RuntimeRows` stays anyway, so rows of every runtime depend on the same kind of service: `RuntimeRowsPlugin`, which accepts only a `RuntimeResolver` now, takes a small trait instead (the runtime name, and taking the rows to refresh), which `GoResolver` implements per runtime; the rows to refresh are those whose manifest changed after the process started.

### 8.3 `GoRuntimes`: start on demand, stop when idle

Also in rutis-loader (it uses `Loader`), mounted after the loader:

```rust
let go = GoResolver::new(GoBinaries::new().dir("plugins/go").file("bin/weather"))
    .with_catalog(&catalog);
let go = Arc::new(go);
chain = chain.with_shared(go.clone());
// ... mount the loader ...
root.plugin(GoRuntimes::new(go, project).idle(Duration::from_secs(60)));
```

- each running Go runtime is a child `Ctx` of `GoRuntimes`, holding `LocalRuntime::go(binary, project).named(runtime name)` and its `RuntimeRowsPlugin`. Stopping a runtime disposes that child `Ctx` and touches no other runtime;
- **Start**: when `GoResolver` resolves a row whose runtime is not running, the runtime starts. The row is waiting for `RuntimeRows#<runtime name>` meanwhile, and starts as usual once the runtime is up. Runtimes starting cold start in parallel;
- **Stop**: when a runtime's last row unloads, wait `idle`; then, if no enabled entry in `Loader::entries()` resolves to the runtime, stop it. A row resolving to it in the meantime cancels the stop;
- **Crashes**: when a process exits unexpectedly, its rows stop and the runtime's state records why, as now, and **it is not restarted automatically**. Rows resolving to it later do not restart it either, until the application calls `GoRuntimes::restart(name)` or the binary file changes (a new build is worth another try);
- **All at once**: `GoRuntimes::eager()` starts every listed binary when mounted, for deployments that do not want a first use to wait for a process start;
- **State**: `GoRuntimes::runtimes()` lists each binary's runtime name, state (not started / starting / running / stopped + why), plugins and versions.

### 8.4 rutis-bridge

| Change | Where |
| --- | --- |
| `Launcher::go(binary, project)`: the program is the binary itself, `cwd` is `project`, `inherit_fd` on Unix | `rutis-bridge/src/runtime/process.rs` |
| `LocalRuntime::go(binary, project)`, named after the file by default (§8.2) | `rutis-bridge/src/runtime/local.rs` |
| Cargo feature `go` (rutis-bridge, rutis-loader); `interop` includes it; CI checks a build with only `go` | both `Cargo.toml`s, `ci.yml` |
| `RowSchema` carries `version`, written into the row's meta | the Bun design's B1 needs this too; whichever lands first does it |
| the remote node runtime's `Naming::Npm` refuses names with a known runtime prefix: `go:` joins the known prefixes | `rutis-loader/src/runtime.rs` |

A Rust application running one fixed binary can skip `GoResolver` and use `LocalRuntime::go` with `RuntimeResolver::modules` (rows `<runtime name>:<plugin name>`), as for Python.

### 8.5 Remote Go runtimes

A Go runtime on another machine has no local file to read a manifest from, so it works like the existing remote runtimes: each is a `RuntimePlugin::remote(name)` linked through a `rutis-bridge/peer` row, rows are `<name>:<plugin name>`, and declarations come from `rows.schema`. A remote machine running several Go binaries is several remote runtimes.

## 9. rutis-host and installation

`rutis.json`:

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

- `dir`, `binaries`: where binaries are found; at least one;
- `start`: `on-demand` (default) or `eager`; `idle`: seconds idle before stopping;
- `remote` accepts `language: "go"`.

Commands:

| Command | Effect | Phase |
| --- | --- | --- |
| `rutis-host check` | lists each binary: runtime name, whether its platform matches, SDK and plugin API versions, plugins; ambiguous plugin names; which binary each row resolves to | G2 |
| `rutis-host dev` | without `rutis.dev.json`, a `go.mod` makes it a Go project (`project.rs` picks Node as soon as it sees `package.json` now; the Bun design changes this too) | G2 |
| `rutis-host new <name> --lang go` | template: plugin package, `cmd/<name>/main.go`, tests, `rutis.dev.json`, a release workflow (on tag: `go test`, `rutis-host check`, binaries per platform with GoReleaser or a `go build` matrix) | G2 |
| `rutis-host go add <module>@<version>` | `go install` into `dir` with the machine's Go toolchain; without one, an error describing the other two ways | G2 |
| `rutis-host go add <URL>` | download an author's prebuilt binary into `dir`, checking the SHA-256 the release gives | G3 |

On macOS, binaries downloaded from the network carry the quarantine attribute, and the system refuses to run them unsigned. `check` and startup detect this (reusing the dylib quarantine check) and report the reason and what to do; the host does not remove the attribute itself.

`rutis-host run` only runs existing binaries and needs no Go toolchain.

## 10. New code

| Case | How it takes effect |
| --- | --- |
| Development (`rutis-host dev`) | watches the project's `.go` files, `go.mod` and `go.sum`; on a change, builds the project's `cmd/<name>` to a **new file** in a cache directory (`<name>-<n>`), and once that succeeds, has `GoResolver` replace the old file with it and restarts that one runtime; the old file is deleted afterwards. A failed build prints the compiler errors and the old process keeps running. Other Go runtimes are not affected |
| Deployment | the deployer puts the new binary in place of the old one and calls `GoRuntimes::restart(name)` (with rutis-host, restarts the host). rutis does not watch binaries |

Builds go to new file names because Windows does not allow overwriting a running executable.

Restarting one runtime has the effect of its process exiting: its rows stop, and so do rows using their services; once the new process is up, the rows resolve from the new manifest and start again in dependency order. The other binaries' runtimes keep running.

## 11. Repository and release

- The SDK lives in `go/rutis`, module path `github.com/arcships/rutis/go/rutis`, next to `python/rutis` and `node/rutis`. It follows the release train with tags like `go/rutis/v0.9.0` (Go's rule for modules in subdirectories); releasing is pushing the tag, and the Go module proxy fetches it;
- standard library only. Go versions: the two current stable releases (stated in `go.mod`);
- packages: `rutis` (plugin API, `Serve`, manifest) and `rutis/rutistest` (testing without a host); the session layer is `internal/peer`, with no promises outside;
- **Compatibility**: between a host and a binary there are only the wire protocol, the manifest format and the plugin API version. Within one protocol version, a binary built with any Go SDK version runs on any host version, as long as its plugin API is not above what the host supports. This matters more for Go than for Node or Python: a published binary does not upgrade along with the host;
- a guide `docs/guide/go-plugin.md` (Chinese and English), and a Go column in the plugin API document.

## 12. Tests

| Test | Where | What it shows |
| --- | --- | --- |
| Session layer unit tests | `go/rutis/internal/peer` (`go test`) | frame encoding and decoding; reference counting and release; callbacks arriving during a synchronous call; cancellation; `path` taken from ctx; concurrent calls |
| Session conformance | `rutis-bridge/tests/runtime_conformance.rs`: a Go `conformance` target (as `conformance-session.mjs` / `conformance_session.py`) | the Rust session conformance suite (`session::testing::session`) passes against Go |
| Runtime contract × channels | `rutis-bridge/tests/session_matrix.rs`: `Runtime` gains `Go` | the runtime conformance suite (`runtime::testing::runtime`) passes over `fd:3` and a dialed-back socket; WebSocket in G2 |
| Plugin tests without a host | `rutistest.Load(t, weather.Plugin, config, services)` | as `rutis.testing`: only services in `Inject` are used; provided services carry their declared methods; cleanups ran when the test ends; in strict mode values are encoded and decoded per §5.1 |
| Manifest | `go/rutis` + `rutis-loader/tests/go_rows.rs` | the manifest matches the running process's `rows.schema`; an unchanged file is not executed again; a too-new plugin API and a platform mismatch give clear errors |
| Several binaries | `rutis-loader/tests/go_rows.rs` | two binaries: rows find their binary by plugin name; a plugin name in both is ambiguous and `go:<binary name>/<plugin name>` works; plugins in the two binaries call each other synchronously and asynchronously; one process crashing stops only its rows and their users, the other runs on |
| Start on demand | same | a binary no row uses is not started; after the last row is removed and the idle period passes, the process exits; a row added meanwhile keeps it; no automatic restart after a crash, but a changed file starts it at the next resolution |
| Runtime conformance | `crates/rutis-loader/tests/multilang.rs`, with feature `go` | the same leaf plugins once more in Go; three languages start cold together and call each other; removing a provider stops only its users; `inject` gates |
| Go versions of the Python runtime tests | in `rutis-bridge/tests`, a Go version of `python_runtime.rs`, and a Go column in `cancellation.rs`, `error_shape.rs`, `rpc_callbacks.rs`, `process_exit.rs`, `live_objects.rs` | features complete; cancellation reaches the method's ctx; error shapes round-trip (Go error type names, `Panic`); services withdrawn when a panic exits the process; references and release. The conformance fixtures `conformance-session`, `conformance-weather`, `conformance-greeter` get Go versions, `crash()` exiting with status 17 |
| Remote leases (G2) | a Go column in `leases.rs`, `remote_rows.rs` | the lease scenarios of the Bun design's §8 |
| Crossing synchronous calls | same | Node calls Go synchronously and the Go method synchronously calls back a function Node passed (with ctx): no deadlock; Go and Python, and two Go runtimes, call each other synchronously: no deadlock |
| Instances | `instance_runtimes.rs` | Go rows inside instances: label isolation, each instance's own services |
| Windows | the existing Windows CI | the Go runtime on a loopback channel; `dev` replacing the file and restarting |

The Go test plugins live in `crates/rutis-loader/tests/fixtures/go` and are built into two or more binaries when the tests start.

CI: a `runtimes-go` job on Linux and macOS installs the two current stable Go releases with `actions/setup-go` and runs `go test ./...`, `cargo test -p rutis-bridge --features go,…` and `cargo test -p rutis-loader --features go,…`; a single-feature build check `cargo check --no-default-features --features go`; Windows joins the existing `runtimes-windows`.

E2E: S2 ([#186](https://github.com/arcships/rutis/issues/186)) gains the `new --lang go` development loop; S3 ([#187](https://github.com/arcships/rutis/issues/187)) Go rows in cross-language composition and crash recovery; S9 ([#193](https://github.com/arcships/rutis/issues/193)) "a downloaded Go binary runs in a clean environment with no Go toolchain".

## 13. Phases

| Phase | Content | Done when |
| --- | --- | --- |
| G1 | `go/rutis`: session layer, leaf SDK, `Serve` (`fd` / `unix` / `tcp` channels), manifest, `rutistest`. Rust: `Launcher::go` / `LocalRuntime::go`, feature `go`, `GoResolver` (files and directories, ambiguity, manifest cache), `GoRuntimes` (only `eager` at first), `RuntimeRowsPlugin` taking a trait | every test in §12 except start on demand and remote passes; several binaries run at once; Linux, macOS and Windows |
| G2 | start on demand and idle stop; rutis-host: `runtimes.go`, rebuild and restart in `dev`, `check`, `new --lang go`, `go add <module>@<version>`; the guide; remote runtimes (`listen:wss://`) | the start-on-demand tests pass; a project made with `rutis-host new --lang go` restarts only its own runtime after an edit under `dev`; `rutis-host` controls a Go runtime on another machine |
| G3 (on demand) | downloading prebuilt binaries with checksums; receiving object references (declaring `objects` in the handshake); helping deployers generate a composing `main.go` | decided when there is a real need |

G1 does not depend on the rutis-host changes and can be used from a Rust host first.

## 14. Open questions

- **How the manifest is obtained.** This document executes the binary (`--rutis-manifest`), at the cost of running Go packages' `init`. The alternative is writing the manifest into the binary after a fixed marker at build time, for the host to read from the file without executing anything (like the dylib metadata section); but configuration schemas come from reflection, which would need an extra build step (`go generate`). Executing comes first; the alternative if someone needs "list plugins without running code".
- **How long before an idle runtime stops**: 60 seconds is a guess. The default could also be never (start on demand only), with idle stopping opt-in.
- **Whether `run` may build**: not in this document, as the overall design says the host does not compile at run time.
- **The default method shape**: `async` here (§5.2).
- **Typed use beyond function fields**, such as bindings generated from the provider's Go interface (`go generate`).
- **Receiving object references**: G3.
- **Detecting a lost call chain** (§6.2): a warning in development mode only.
- **Whether Python and Node want several runtimes started on demand too**: they are distributed into environments and do not have the same need; not covered here.
