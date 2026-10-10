# Writing a Bun plugin

The plugin runs in [Bun](https://bun.com): the host starts the Bun runtime `@arcships/rutis-bun`, and rows are named `bun:<module>`. Plugins are written as [TypeScript plugins](typescript-plugin.en.md) are (`definePlugin`, see the [plugin API](plugin-api.en.md)); the difference is that Bun runs them, so TypeScript needs no build.

## 1. Create a project

```bash
bunx --bun @arcships/rutis-host new greeter --lang bun
cd greeter
bun install
```

| File | Purpose |
| --- | --- |
| `src/index.ts` | The plugin |
| `test/index.test.ts` | Unit tests (`bun test`), no host needed |
| `rutis.dev.json` | Local runtime configuration and other plugins for testing |
| `package.json` | Depends on `@arcships/rutis`; dev dependencies `@arcships/rutis-bun`, `@arcships/rutis-host` |
| `tsconfig.json` | `bun run check` type checks: Bun runs TypeScript without checking types |
| `.github/workflows/publish.yml` | Tests and publishes to npm on a `v*` tag |

## 2. Write and test the plugin

Write it as in sections 2 and 3 of the TypeScript plugin guide. Tests use `bun test`:

```ts
import { expect, test } from 'bun:test'
import { load } from '@arcships/rutis/testing'
import plugin from '../src/index.ts'

test('greets', async () => {
  const t = await load(plugin, { config: { greeting: 'Hi' } })
  expect(t.service('greeter').hello('Ada')).toBe('Hi, Ada!')
  await t.unload()
})
```

Without the SDK, a module can also export `apply(ctx, config)` directly, with `inject`, `provides` (each method `'sync'` or `'async'`) and `config` (a JSON Schema).

## 3. Run it in a host

```bash
bunx --bun rutis-host dev
```

`rutis-host dev` runs the project in Bun when it finds `bun.lock`, `runtimes.bun` in `rutis.dev.json`, or a dependency on `@arcships/rutis-bun`, and reloads it when files change.

A host's `rutis.json`:

```json
{
  "runtimes": { "bun": { "project": "." } },
  "rows": [
    { "id": "greeter", "name": "bun:greeter" },
    { "id": "report", "name": "bun:./report.ts", "inject": ["greeter"] }
  ]
}
```

- `bun:<npm name or subpath>` resolves from the `node_modules` of `project`; `bun:./path` is relative to `project`. The row `bun:sqlite` names the project's module `sqlite`, not Bun's built-in `bun:sqlite`.
- Fields of `runtimes.bun`: `project` (default `.`), `runtime` (where `@arcships/rutis-bun` is; by default the project's), `program` (the Bun executable; by default `bun` on `PATH`).
- It can be configured together with `runtimes.node` and `runtimes.py`; services are shared across runtimes by name.

## 4. What the runtime promises

- **Only what the project installed**: the runtime always starts with `--no-install`; a missing package makes loading fail and is never downloaded. Install plugins and their dependencies with `bun add`.
- **No `.env`**: the runtime starts with `--no-env-file`, so the environment comes only from the host. The project's `bunfig.toml` applies (including `preload`), as the project's own trusted configuration.
- **Re-entrant synchronous calls**: while a plugin's synchronous call into rutis waits, other calls into this runtime still run. A plugin's service may therefore be called during its own synchronous call: do not hold a lock across a call into rutis.
- **An uncaught error ends the process**, withdrawing every service of the runtime.
- **Reloading** imports the plugin module itself again; the modules it imports are replaced only by restarting the runtime.
- **Packages installed while the runtime runs** (`bun add`) are found only after the runtime restarts: a Bun process remembers that a package was missing.

## 5. Publish

As for TypeScript plugins: `npm publish` (the template's workflow publishes on a `v*` tag). A host installs it with `bun add` and adds a row `{ "id": "greeter", "name": "bun:greeter" }`.

## Environment

Bun 1.4 or later. This version of the Bun runtime runs on the host's machine; listening as a remote runtime (`listen:`) is not supported yet. Windows is not tested yet.
