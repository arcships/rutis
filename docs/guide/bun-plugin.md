# 写一个 Bun 插件

插件在 [Bun](https://bun.com) 里运行：宿主启动 Bun 运行时 `@arcships/rutis-bun`，行名写作 `bun:<模块>`。插件的写法与 [TypeScript 插件](typescript-plugin.md)相同（`definePlugin`，见 [插件 API](plugin-api.md)），区别在于由 Bun 运行，TypeScript 不需要编译。

## 1. 创建项目

```bash
bunx --bun @arcships/rutis-host new greeter --lang bun
cd greeter
bun install
```

| 文件 | 作用 |
| --- | --- |
| `src/index.ts` | 插件本身 |
| `test/index.test.ts` | 单元测试（`bun test`），不需要宿主 |
| `rutis.dev.json` | 本地运行时的配置和测试用的其他插件 |
| `package.json` | 依赖 `@arcships/rutis`；开发依赖 `@arcships/rutis-bun`、`@arcships/rutis-host` |
| `tsconfig.json` | `bun run check` 做类型检查：Bun 运行 TypeScript 时不检查类型 |
| `.github/workflows/publish.yml` | 打 `v*` tag 时测试并发布到 npm |

## 2. 写插件与测试

写法与 TypeScript 插件第 2、3 节相同。测试用 `bun test`：

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

不用 SDK 时，模块也可以直接导出 `apply(ctx, config)`，以及 `inject`、`provides`（每个方法写明 `'sync'` 或 `'async'`）、`config`（JSON Schema）。

## 3. 在宿主里运行

```bash
bunx --bun rutis-host dev
```

`rutis-host dev` 看到 `bun.lock`、`rutis.dev.json` 里的 `runtimes.bun`，或依赖 `@arcships/rutis-bun`，就用 Bun 运行这个项目，文件变化时重新加载。

宿主的 `rutis.json`：

```json
{
  "runtimes": { "bun": { "project": "." } },
  "rows": [
    { "id": "greeter", "name": "bun:greeter" },
    { "id": "report", "name": "bun:./report.ts", "inject": ["greeter"] }
  ]
}
```

- `bun:<npm 名或子路径>` 从 `project` 的 `node_modules` 解析，`bun:./路径` 相对于 `project`。行名 `bun:sqlite` 指项目里的模块 `sqlite`，不是 Bun 的内置模块 `bun:sqlite`。
- `runtimes.bun` 的字段：`project`（默认 `.`）、`runtime`（`@arcships/rutis-bun` 的位置，默认项目里装的）、`program`（Bun 可执行文件，默认 `PATH` 上的 `bun`）。
- 可以和 `runtimes.node`、`runtimes.py` 同时配置；服务按名字跨运行时共享。

## 4. 运行时的约定

- **只用项目里装好的包**：运行时总是带 `--no-install` 启动，缺少的包会让加载失败，不会被下载。用 `bun add` 安装插件和它的依赖。
- **不读 `.env`**：运行时带 `--no-env-file` 启动，环境变量只来自宿主。项目的 `bunfig.toml` 照常生效（包括 `preload`），它是项目自己的可信配置。
- **同步调用可重入**：插件同步调用 rutis 服务、等待结果时，别的调用仍会进入这个运行时执行。所以插件的服务可能在它自己的同步调用中途被调用：调用 rutis 服务时不要持有锁。
- **未捕获的错误结束进程**：这个运行时的所有服务随之撤销。
- **重新加载**只重新导入插件模块本身；它导入的其他模块要重启运行时才更新。
- **运行期间新装的包**（`bun add`）要重启运行时才能被找到：Bun 进程会记住某个包曾经不存在。

## 5. 发布

与 TypeScript 插件相同：`npm publish`（模板的 workflow 在打 `v*` tag 时发布）。宿主用 `bun add` 安装后，加一行 `{ "id": "greeter", "name": "bun:greeter" }`。

## 环境

Bun 1.2 或更高。本版本的 Bun 运行时在宿主所在的机器上运行；作为远程运行时监听（`listen:`）尚不支持。Windows 尚未测试。
