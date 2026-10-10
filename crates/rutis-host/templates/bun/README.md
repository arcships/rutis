# {{name}}

A [rutis](https://github.com/arcships/rutis) plugin, run by the Bun runtime.

```bash
bun install
bun test                   # unit tests, no host needed
bun run check              # type check (Bun runs TypeScript without checking it)
bunx --bun rutis-host dev  # run it in a local host; changes reload it
```

Publish with `npm publish` (the workflow in `.github/workflows/publish.yml` does it for a `v*` tag). A host installs it next to `@arcships/rutis-bun`, configures `"runtimes": { "bun": {} }` and adds a row `{ "id": "{{id}}", "name": "bun:{{name}}" }`.
