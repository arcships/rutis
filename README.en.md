<div align="center">

# rutis

**A plugin framework for Rust**

Type-safe service container · fiber lifecycles · four-way event bus · dependency-driven hot reloading

[![crates.io](https://img.shields.io/crates/v/rutis.svg)](https://crates.io/crates/rutis)
[![docs.rs](https://docs.rs/rutis/badge.svg)](https://docs.rs/rutis)
[![CI](https://github.com/arcships/rutis/actions/workflows/ci.yml/badge.svg)](https://github.com/arcships/rutis/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/crates/l/rutis.svg)](LICENSE)
![Rust 1.85+](https://img.shields.io/badge/rust-1.85%2B-orange)

An idiomatic Rust implementation of the Cordis core paradigm · [中文](README.md)

</div>

## ✨ Why

When your application needs a plugin architecture — editors, bots, agent hosts, composable servers — rolling your own means hand-writing a pile of error-prone infrastructure. rutis turns all of it into declarations:

| Hand-rolled pain | What rutis gives you |
|---|---|
| String-keyed services scattered everywhere | **Compile-time typed keys**: `ctx.get::<Database>()` — wrong types don't compile |
| Implicit plugin start/stop ordering conventions | **One apply, everything wired**: provides services / listeners / cleanup, exactly once |
| Resource leaks and missed cleanups on unload | **Fiber containers**: strict LIFO cleanup, rolling back even mid-apply failures |
| Manually rebuilding a chain of things when a dependency changes | **Dependency-driven reload**: swap a provider, consumers evict and reload themselves |
| Restarting the whole process to change config | **Config hot update**: `update(config)` unloads and reloads cleanly, downstream follows |

## 🚀 Getting started

```bash
cargo add rutis@0.2
```

A provider, a consumer that declares a dependency, and a provider swap — full code at [crates/rutis/examples/quickstart.rs](crates/rutis/examples/quickstart.rs) (`cargo run -p rutis --example quickstart`):

```rust
use rutis::{BoxFuture, CordisError, Ctx, Effect, Plugin, TypeKey};

/// A service: the type is the key; one registration slot per type.
struct Greeting(String);

/// Provider: provides the service in apply; the framework removes it on unload.
struct Greeter { version: u32 }

impl Plugin for Greeter {
    fn name(&self) -> &str { "greeter" }
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, Result<Effect, CordisError>> {
        let greeting = Greeting(format!("hello from greeter v{}", self.version));
        Box::pin(async move {
            ctx.provide(greeting)?;
            Ok(Effect::Done)
        })
    }
}

/// Consumer: declares a dependency on Greeting — once declared, the
/// framework owns when it loads.
struct Listener { deps: Vec<TypeKey> }

impl Plugin for Listener {
    fn name(&self) -> &str { "listener" }
    fn injects(&self) -> &[TypeKey] { &self.deps }  // stays Pending until ready
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, Result<Effect, CordisError>> {
        let greeting = ctx.get::<Greeting>().unwrap().0.clone();
        Box::pin(async move {
            println!("[listener] loaded: {greeting}");
            Ok(Effect::Done)
        })
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let ctx = Ctx::root()?;
    let listener = ctx.plugin(Listener {
        deps: vec![TypeKey::of::<Greeting>()],
    });
    // Provider arrives → gate opens, consumer loads automatically.
    let v1 = ctx.plugin(Greeter { version: 1 });
    wait_active(&listener).await;                 // [listener] loaded: hello from greeter v1

    // Swap the provider: old one unloads, the consumer is evicted, the new
    // one provides → automatic reload. The consumer is never touched.
    v1.dispose().await?;
    let _v2 = ctx.plugin(Greeter { version: 2 });
    wait_active(&listener).await;                 // [listener] loaded: hello from greeter v2
    Ok(())
}
```

## 🧩 Core concepts: the five pillars

| Pillar | What it is |
|---|---|
| **Plugin = unit of assembly** | one `apply` provides services / listeners / cleanup |
| **Fiber = lifecycle container** | six-state machine + dependency gating + cascading unload + exactly-once cleanup |
| **Service = typed registry** | isolate scopes; multiple instances of one interface via qualified keys |
| **Event bus = four dispatch semantics** | emit (ordered per key) / parallel (concurrent fan-out) / serial (first-value short-circuit) / waterfall (middleware chain) |
| **Dependency-driven reload** | provider unload → consumers evicted and reloaded automatically |

The mental model in one line: **declare dependencies → gated loading → provider changes → consumers reload themselves**.

Fiber lifecycle (failed loads roll back atomically; Failed is sticky until deps return or a hot update):

```mermaid
stateDiagram-v2
    [*] --> Pending : spawn (deps missing)
    Pending --> Loading : deps ready
    Loading --> Active : apply ok
    Loading --> Failed : validate / apply failed (half-registered resources rolled back)
    Active --> Unloading : deps gone / dispose / update
    Failed --> Unloading : restart / update / deps restored
    Unloading --> Pending : cleanup done (exactly once, LIFO)
    Unloading --> Disposed : terminal dispose
    Disposed --> [*]
```

The full reload sequence (step 3 of the example above):

```mermaid
sequenceDiagram
    participant App
    participant R as Registry (typed keys)
    participant P as Provider fiber
    participant C as Consumer fiber
    App->>P: dispose()
    P->>R: evict service
    R-->>C: evict: pre-cancel + dependency re-check
    C->>C: Active → Pending
    App->>P: plugin(Greeter v2)
    P->>R: provide Greeting
    R-->>C: deps-ready notification
    C->>C: Pending → Loading → Active (apply again)
```

## ⚡ Feature tour

**Config hot update** — change config at runtime, reusing the state machine's exactly-once cleanup; affected consumers follow automatically:

```rust
struct MyFactory;
impl PluginFactory<MyConfig> for MyFactory {
    fn build(&self, cfg: &MyConfig) -> Result<Box<dyn Plugin>, CordisError> { /* ... */ }
}

let view = ctx.plugin_with(MyFactory, cfg_v1);
view.update(cfg_v2).await?;   // dry-run failure leaves everything untouched; success unloads + reloads
```

**Dynamic event names** — events whose names are only known at runtime (host events, script-registered channels): typed events + dynamic qualifiers inherit all four dispatch semantics and lifecycle cleanup for free:

```rust
ctx.events().on_keyed::<HostEvent>(&ctx, "session/event", listener)?;
ctx.events().emit_keyed(&ctx, name, Arc::new(event));
```

**Relation to cordis** — rutis is an idiomatic Rust implementation of the [Cordis](https://github.com/shigma/cordis) paradigm, not a translation: all 96 original specs reviewed line by line, the 58 language-agnostic invariants locked by automated parity tests; every other difference is explicitly declared (decision table + non-port list + audit record). Known deliberate strengthenings: cross-effect cleanup is strictly serial LIFO (cordis runs concurrently), per-key emit ordering is rebuilt explicitly.

## 🛠 Built with rutis

| Project | Description |
|---|---|
| [rutis-agent](crates/rutis-agent) / [rutis-cli](crates/rutis-cli) | A minimal coding agent sample: aimux `LanguageModel` service + tool plugin + streaming driver plugin + ratatui TUI (build from source; crates.io 0.1.0 is the older pre-rutui version) |
| [rutis-dsh](crates/rutis-dsh) + [host/](host) | A bridge feeding LLM services to the dsh host process: Rust composition root ↔ loopback TCP ↔ TS bridge plugin; host events flow `evt/emit` → `HostEvent` into the kernel bus |
| [aimux-llm](crates/aimux-llm) | A standalone LLM service plugin: apply → registers the `llm` service, 329 providers |

Sample commands inside this repo:

```bash
cargo run -p rutis-cli -- --scripted          # offline agent demo, no API key
cargo run -p rutis-agent --example tui_scripted   # scripted-backend TUI
cargo test                                    # full test suite
```

> agent / cli consume [aimux](https://crates.io/crates/aimux-core) (unified LLM access layer) from crates.io — no sibling checkout needed; to hack a local aimux, add an uncommitted `[patch]` at the workspace root.

## 📚 Documentation

**Kernel & paradigm** — [kernel design (D1–D31 decision table)](docs/design-rust-port.md) · [96-spec parity ruling](docs/cordis-spec-parity-2026-08-18.md) · [hot update + dynamic events (design / three review rounds / post-mortem / audit)](docs/design-config-hot-update-and-dynamic-events-2026-09-21.md)

**Bridge & host** — [dual-core architecture & rustification roadmap](docs/design-dual-core-2026-08-20.md) · [dsh bridge v1 design](docs/design-dsh-bridge-2026-08-21.md) · [aimux-llm plugin ruling](docs/decision-aimux-llm-plugin-2026-08-23.md)

**Agent** — [agent framework](docs/design-min-agent-2026-08-18.md) · [verification & TUI](docs/design-agent-verification-tui-2026-08-18.md) · [minimal mode](docs/design-minimal-mode-2026-08-18.md)

## License

MIT (inherited from [Cordis](https://github.com/shigma/cordis) © Shigma).
