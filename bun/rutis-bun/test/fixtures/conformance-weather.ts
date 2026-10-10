// The runtime conformance plugin (rutis_bridge::runtime::testing::runtime),
// for the Bun runtime.
export const inject = ['clock']
export const provides = { weather: { today: 'sync', later: 'async', each: 'sync', crash: 'sync' } }
export function apply(ctx: { use(name: string): any, provide(name: string, value: unknown): void }) {
  const clock = ctx.use('clock')
  ctx.provide('weather', {
    today() { return `Oslo at ${clock.now()}` },
    async later() { return 'Oslo later' },
    each(callback: (day: string) => string) { return ['mon', 'tue'].map(day => callback(day)) },
    crash() { process.exit(17) },
  })
}
