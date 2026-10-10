// The e2e probe for the Node runtime: calls the services the harness asks
// for and prints each result as one JSON line on stdout.
//
// The harness writes a request as `<seq>.json` into `config.dir`:
// `{ "service": "report", "method": "summary", "args": ["greeting"] }`. The
// probe answers `{"probe":"<id>","seq":<seq>,"ok":<result>}` or
// `{"probe":"<id>","seq":<seq>,"error":"<message>"}`, and reports
// `{"probe":"<id>","event":"started"}` / `"stopped"` as it loads and unloads.
// The harness writes one copy per probe, with the services it calls in
// place of `INJECT` (a plugin declares what it uses), so it starts once they
// run.
import { definePlugin } from '@arcships/rutis'
import { readdirSync, readFileSync, rmSync } from 'node:fs'
import { join } from 'node:path'

interface Config {
  id: string
  dir: string
}

interface Request {
  service: string
  method: string
  args: unknown[]
}

type Service = Record<string, (...args: unknown[]) => unknown>

export default definePlugin<Config>({
  inject: INJECT,
  apply(ctx, config) {
    const say = (record: object) => console.log(JSON.stringify({ probe: config.id, ...record }))
    let busy = false
    const poll = async () => {
      if (busy) return
      busy = true
      try {
        const names = readdirSync(config.dir)
          .filter(name => name.endsWith('.json'))
          .sort((a, b) => Number.parseInt(a) - Number.parseInt(b))
        for (const name of names) {
          const path = join(config.dir, name)
          const request: Request = JSON.parse(readFileSync(path, 'utf8'))
          rmSync(path)
          const seq = Number.parseInt(name)
          try {
            const service = ctx.use<Service>(request.service)
            say({ seq, ok: (await service[request.method](...request.args)) ?? null })
          } catch (error) {
            say({ seq, error: String((error as Error)?.message ?? error) })
          }
        }
      } finally {
        busy = false
      }
    }
    const timer = setInterval(poll, 50)
    say({ event: 'started' })
    return () => {
      clearInterval(timer)
      say({ event: 'stopped' })
    }
  },
})
