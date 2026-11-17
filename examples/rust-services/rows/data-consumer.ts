import type { Calendar, Change, Entry } from './services.js'

type Data = null | boolean | number | bigint | string | Data[] | { [key: string]: Data | undefined }

export async function readReply(calendar: Calendar, id: number | bigint): Promise<Data> {
  return { entry: await calendar.get(id) }
}

export function forwardChanges(changes: Change[]): Data {
  return { changes }
}

export function bigIntegerEntry(): Data {
  const entry: Entry = {
    id: 9_007_199_254_740_993n,
    title: 'Standup',
    startsAt: 9_007_199_254_740_993n,
    tags: ['team'],
    kind: 'meeting',
    note: null,
  }
  return entry
}

export function rejectService(calendar: Calendar): void {
  // @ts-expect-error Service methods are not data.
  const data: Data = calendar
  void data
}
