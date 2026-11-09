#!/usr/bin/env node
// Rule trace table: docs/formal-trace/ (§5 of
// docs/design-formal-verification-2026-11-08.md, issue #634).
//
//   node tools/formal-trace.mjs            counts, then checks; exit 1 on a problem
//   node tools/formal-trace.mjs --write    regenerate the guarantee rows first
//   node tools/formal-trace.mjs --check    checks only (no counts)
//
// The guarantee rows (rules/guarantees-*.md) are the register's rows
// (docs/guarantees.md, §6–§15), one row per register row, extracted from the
// Markdown tables and never written by hand: `--write` rewrites the automatic
// columns (id, source, plain-language promise, level, evidence, status), keeps
// the manual ones (scope, mutation or counterexample, gap) by id, and refreshes
// the index in README.md between <!-- index:begin --> and <!-- index:end -->.
// `--check` reports a row that drifted from the register, a row the register
// has and the table does not (and the reverse), a status or level outside the
// vocabulary, a duplicate id, and an evidence path, test name or line range
// that does not resolve. The trace rows (rules/trace-contract.md,
// rules/event-close-admission.md) are written by hand and checked the same way.
import { existsSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs'
import { posix } from 'node:path'
import { fileURLToPath } from 'node:url'

const root = fileURLToPath(new URL('../', import.meta.url))
const read = file => readFileSync(`${root}${file}`, 'utf8')
const exists = file => existsSync(`${root}${file}`)
const lineCount = file => read(file).split('\n').length

const REGISTER = 'docs/guarantees.md'
const RULES = 'docs/formal-trace/rules'
const INDEX = 'docs/formal-trace/README.md'
const INDEX_BEGIN = '<!-- index:begin -->'
const INDEX_END = '<!-- index:end -->'

// The columns of a row, in the order §5 lists the fields.
const COLUMNS = ['编号', '现行来源', '人话承诺', '级别', '适用范围', '证据', '变异或反例', '状态', '缺口']

// One file per guarantee family: [file, title, families, register sections].
const FILES = [
  ['guarantees-core.md', '`core/*`', ['core'], '§6、§7'],
  ['guarantees-events.md', '`events/*`', ['events'], '§7、§8a'],
  ['guarantees-boundary.md', '`boundary/*`', ['boundary'], '§8'],
  ['guarantees-instance.md', '`instance/*`', ['instance'], '§9'],
  ['guarantees-bridge.md', '`bridge/*`', ['bridge'], '§10'],
  ['guarantees-host.md', '`host/*`、`dsh/*`', ['host', 'dsh'], '§11、§13、§15'],
  ['guarantees-sdk.md', '`sdk/*`', ['sdk'], '§12'],
  ['guarantees-loader.md', '`loader/*`、`swift/*`', ['loader', 'swift'], '§14'],
]
const HANDWRITTEN = ['trace-contract.md', 'event-close-admission.md']

const LEVELS = ['核心承诺', '契约行为', '明确不保证', '—']
const STATUSES = ['已验证', '部分验证', '未验证', '明确不保证', '已撤回', '不单独计数']

// Cells of a Markdown table row; `\|` stays escaped, as the register writes it.
const cellsOf = line => line.slice(1, -1).split(/(?<!\\)\|/).map(cell => cell.trim())

// A cell as it is written: one line, `|` escaped.
const cell = text => String(text ?? '').replace(/\r?\n/g, ' ').replace(/(?<!\\)\|/g, '\\|').trim()

// The rows of a table whose first column is an id in code spans.
function table(text) {
  const rows = []
  let header = null
  for (const [index, line] of text.split('\n').entries()) {
    if (!line.startsWith('|')) {
      if (line.trim()) header = null
      continue
    }
    const found = cellsOf(line)
    if (['编号', 'ID', 'Id'].includes(found[0])) { header = found; continue }
    if (!header || /^-+$/.test(found[0])) continue
    const id = /^`([^`]+)`$/.exec(found[0])?.[1]
    if (!id) continue
    const row = { id, line: index + 1 }
    for (const [column, name] of header.entries()) row[name] = found[column] ?? ''
    rows.push(row)
  }
  return rows
}

// The register rows, in file order, with the section they sit under.
function register() {
  const rows = []
  let section = null
  let header = null
  for (const [index, line] of read(REGISTER).split('\n').entries()) {
    const heading = /^## (.+)$/.exec(line)
    if (heading) {
      section = heading[1]
      header = null
      continue
    }
    if (!line.startsWith('|')) {
      if (line.trim()) header = null
      continue
    }
    const found = cellsOf(line)
    if (['编号', 'ID', 'Id'].includes(found[0])) { header = found; continue }
    if (!header || /^-+$/.test(found[0])) continue
    const id = /^`([^`]+)`$/.exec(found[0])?.[1]
    if (!id) continue
    const at = name => {
      const column = header.indexOf(name)
      return column < 0 ? '' : found[column] ?? ''
    }
    const number = /^(\d+[a-z]?)\./.exec(section ?? '')?.[1] ?? ''
    rows.push({
      id,
      line: index + 1,
      section: number,
      origin: at('出处'),
      content: at('内容'),
      level: at('级别'),
      status: at('状态'),
      evidence: at('验证'),
      note: at('说明') || at('备注'),
    })
  }
  return rows
}

// A link of the register, made to work from rules/: the register's links are
// written in docs/, and rules/ is two levels down (`../specs/…` included).
const relink = text => text.replace(/\]\(([^)\s]+)\)/g, (whole, target) =>
  /^(?:[a-z][a-z0-9+.-]*:|\/|#|\.\.\/\.\.\/)/i.test(target) ? whole : `](../../${target})`)

// The register's status, in the table's vocabulary.
function status(row) {
  if (/^已撤回/.test(row.status)) return '已撤回'
  if (row.status === '—' || !row.status) return '不单独计数'
  return row.status
}

// The automatic columns of one register row.
function automatic(row) {
  return {
    编号: `\`${row.id}\``,
    现行来源: `[guarantees.md](../../guarantees.md) §${row.section}${row.origin ? ` · ${relink(row.origin)}` : ''}`,
    人话承诺: relink(row.content),
    级别: row.level,
    证据: row.evidence || '—',
    状态: status(row),
  }
}

// The manual columns a new row starts with: the register's own note is the gap
// it already names; a verified row without one has none.
function defaults(row) {
  const note = row.note?.trim()
  let gap = '未登记'
  if (/^已撤回/.test(row.status)) gap = note ? `${row.status}；${note}` : row.status
  else if (note) gap = note
  else if (status(row) === '已验证') gap = '—'
  return { 适用范围: '未登记', 变异或反例: '未登记', 缺口: gap }
}

// The manual columns already in a rules file, by id.
function manual(file) {
  const kept = new Map()
  if (!exists(file)) return kept
  for (const row of table(read(file))) {
    if (!exists(file)) continue
    kept.set(row.id, {
      适用范围: row.适用范围 ?? '未登记',
      变异或反例: row.变异或反例 ?? '未登记',
      缺口: row.缺口 ?? '未登记',
    })
  }
  return kept
}

const header = (title, sections) => `# 规则：${title}（保证登记表 ${sections}）

[中文索引](../README.md) · [English](../README.en.md) · 数据来源：[保证登记表](../../guarantees.md)

由 \`node tools/formal-trace.mjs --write\` 从登记表生成：自动列（编号、现行来源、人话承诺、级别、证据、状态）不要手改，手改会被 \`--check\` 报出；人工列（适用范围、变异或反例、缺口）按编号保留。缺口的初始值取登记表的「说明」列。

编号沿用登记表，一经登记不复用；撤销的行保留。

| ${COLUMNS.join(' | ')} |
| ${COLUMNS.map(() => '---').join(' | ')} |`

// The register's links are written in docs/; a link that does not resolve
// from rules/ but does from docs/ gets the `../../` it needs. Idempotent, so
// it also repairs the links copied from the register's notes column.
function repair(body) {
  return body.replace(/\]\(([^)\s]+)\)/g, (whole, target) => {
    if (/^(?:[a-z][a-z0-9+.-]*:|\/|#)/i.test(target)) return whole
    const bare = target.replace(/[#?].*$/, '')
    if (!bare || exists(posix.normalize(posix.join(RULES, bare)))) return whole
    return exists(posix.normalize(posix.join('docs', bare))) ? `](../../${target})` : whole
  })
}

function write() {
  const registerRows = register()
  const changed = []
  mkdirSync(`${root}${RULES}`, { recursive: true })
  for (const [file, title, families, sections] of FILES) {
    const kept = manual(`${RULES}/${file}`)
    const body = registerRows
      .filter(row => families.includes(row.id.split('/')[0]))
      .map(row => {
        const fields = { ...defaults(row), ...automatic(row), ...(kept.get(row.id) ?? {}) }
        return `| ${COLUMNS.map(column => cell(fields[column] ?? '')).join(' | ')} |`
      })
    const content = repair(`${header(title, sections)}\n${body.join('\n')}\n`)
    if (!exists(`${RULES}/${file}`) || read(`${RULES}/${file}`) !== content) {
      writeFileSync(`${root}${RULES}/${file}`, content)
      changed.push(file)
    }
  }
  console.log(changed.length ? `wrote ${changed.join(', ')}` : 'rules files are current')
}

const rowsOf = file => table(read(file)).map(row => ({ ...row, file }))

// Every row of the table: the generated files and the hand-written ones.
function rules() {
  const found = FILES.flatMap(([file]) => (exists(`${RULES}/${file}`) ? rowsOf(`${RULES}/${file}`) : []))
  for (const file of HANDWRITTEN) if (exists(`${RULES}/${file}`)) found.push(...rowsOf(`${RULES}/${file}`))
  return found
}

// Where a path in an evidence cell is: the register writes paths relative to
// crates/, the trace rows from the root. (`rutis-e2e/` is tests/e2e.)
const ROOTS = ['crates/', 'node/', 'bun/', 'python/', 'go/', 'swift/', 'kotlin/', 'tests/', 'tools/', 'specs/', 'docs/', 'examples/', 'fuzz/', '.github/']
function locate(path) {
  if (path.startsWith('rutis-e2e/')) return `tests/e2e/${path.slice('rutis-e2e/'.length)}`
  return ROOTS.some(prefix => path.startsWith(prefix)) ? path : `crates/${path}`
}

// Whether `file` defines `name` (a test, a function, a class).
function defines(file, name) {
  const text = read(file)
  const last = name.split('::').at(-1)
  const plain = last.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')
  const escaped = last.replaceAll("'", "\\'")
  return new RegExp(`\\b(?:fn|def|func|fun|class|mod|struct|test|it|describe|async fn|async def)\\s+${plain}\\b`).test(text)
    || text.includes(`'${escaped}'`) || text.includes(`"${escaped}"`) || text.includes(`\`${escaped}\``)
}

// The `path::test` and `path:line-line` references of one evidence cell; a bare
// `::name` is the test last file named in the same cell. A span inside
// parentheses is a JavaScript test title only when the last file was one, and
// never a path of its own — the register writes fixtures that way.
function evidenceOf(text) {
  const found = []
  let last = null
  let depth = 0
  const source = String(text ?? '')
  for (let at = 0; at < source.length; at++) {
    const char = source[at]
    if (char === '（' || char === '(') { depth++; continue }
    if (char === '）' || char === ')') { depth = Math.max(0, depth - 1); continue }
    if (char !== '`') continue
    const end = source.indexOf('`', at + 1)
    if (end < 0) break
    const token = source.slice(at + 1, end).trim()
    at = end
    if (depth > 0) {
      if (last && /\.(mjs|ts|js)$/.test(last) && token && !token.includes('`')) found.push({ token, file: last, name: token, from: null, to: null })
      continue
    }
    const bare = /^::((?:[A-Za-z0-9_]+::)*[A-Za-z0-9_]+)$/.exec(token)
    if (bare && last) { found.push({ token, file: last, name: bare[1], from: null, to: null }); continue }
    const parsed = /^([^:]+?\.(?:rs|mjs|ts|js|py|go|swift|kt|yml))(?:(?:::)((?:[A-Za-z0-9_]+::)*[A-Za-z0-9_]+)|:(\d+)(?:-(\d+))?)?$/.exec(token)
    if (!parsed) continue
    last = locate(parsed[1])
    found.push({
      token,
      file: last,
      name: parsed[2],
      from: parsed[3] ? Number(parsed[3]) : null,
      to: parsed[4] ? Number(parsed[4]) : parsed[3] ? Number(parsed[3]) : null,
    })
  }
  return found
}

const problems = []
const say = line => problems.push(line)

// The evidence of one row: the file, the test name and the lines.
function evidence(row, counts) {
  for (const item of evidenceOf(row.证据)) {
    counts.references++
    if (!exists(item.file)) {
      say(`${row.file}:${row.line}: ${row.id}: ${item.token}: ${item.file} does not exist`)
      counts.missing++
      continue
    }
    if (item.name) {
      counts.names++
      if (!defines(item.file, item.name)) {
        say(`${row.file}:${row.line}: ${row.id}: ${item.token}: ${item.file} has no ${item.name}`)
        counts.namesMissing++
      }
    }
    if (item.to !== null) {
      counts.ranges++
      const total = lineCount(item.file)
      if (item.to > total) {
        say(`${row.file}:${row.line}: ${row.id}: ${item.token}: ${item.file} has ${total} lines, not ${item.to}`)
        counts.rangesMissing++
      }
    }
  }
}

// Every relative link of a Markdown file of this directory that does not
// resolve, as "file:line: target".
function links(file) {
  const broken = []
  let fence = null
  for (const [index, line] of read(file).split('\n').entries()) {
    const marker = /^\s*(`{3,}|~{3,})/.exec(line)
    if (marker) {
      if (!fence) fence = marker[1]
      else if (marker[1][0] === fence[0] && marker[1].length >= fence.length) fence = null
      continue
    }
    if (fence) continue
    for (const match of line.replace(/`[^`]*`/g, '').matchAll(/!?\[(?:[^\]\\]|\\.)*\]\(\s*(<[^>]*>|[^\s)]+)/g)) {
      const target = match[1].replace(/^<|>$/g, '')
      if (/^(?:[a-z][a-z0-9+.-]*:|#|\/)/i.test(target)) continue
      const path = target.replace(/[#?].*$/, '')
      if (!path) continue
      const resolved = posix.normalize(posix.join(posix.dirname(file), path)).replace(/\/$/, '')
      if (!exists(resolved)) broken.push(`${file}:${index + 1}: ${target}`)
    }
  }
  return broken
}

// The index table: rows per file, by status.
function index(rows) {
  const lines = [
    '| 文件 | 覆盖 | 条数 | 已验证 | 部分验证 | 未验证 | 明确不保证 | 已撤回 | 不单独计数 |',
    '| --- | --- | --- | --- | --- | --- | --- | --- | --- |',
  ]
  const counted = (file, title) => {
    const list = rows.filter(row => row.file === `${RULES}/${file}`)
    const of = status => list.filter(row => row.状态 === status).length
    lines.push(`| [${file}](rules/${file}) | ${title} | ${list.length} | ${of('已验证')} | ${of('部分验证')} | ${of('未验证')} | ${of('明确不保证')} | ${of('已撤回')} | ${of('不单独计数')} |`)
  }
  for (const [file, title] of FILES) counted(file, title)
  for (const file of HANDWRITTEN) counted(file, '手写')
  const whole = status => rows.filter(row => row.状态 === status).length
  lines.push(`| 合计 | — | ${rows.length} | ${whole('已验证')} | ${whole('部分验证')} | ${whole('未验证')} | ${whole('明确不保证')} | ${whole('已撤回')} | ${whole('不单独计数')} |`)
  return lines.join('\n')
}

// The index block of README.md, between the markers.
function indexBlock(text) {
  const begin = text.indexOf(INDEX_BEGIN)
  const end = text.indexOf(INDEX_END)
  if (begin < 0 || end < begin) return null
  return text.slice(begin + INDEX_BEGIN.length, end).replace(/^\n+|\n+$/g, '')
}

function refreshIndex(rows) {
  if (!exists(INDEX)) return
  const text = read(INDEX)
  const block = indexBlock(text)
  if (block === null) return
  const begin = text.indexOf(INDEX_BEGIN) + INDEX_BEGIN.length
  const end = text.indexOf(INDEX_END)
  const wanted = `${text.slice(0, begin)}\n${index(rows)}\n${text.slice(end)}`
  if (wanted !== text) writeFileSync(`${root}${INDEX}`, wanted)
}

const counts = { references: 0, missing: 0, names: 0, namesMissing: 0, ranges: 0, rangesMissing: 0 }

if (process.argv.includes('--write')) write()

const rows = rules()
const registerRows = register()
const byId = new Map(rows.map(row => [row.id, row]))

// 1. Every register row is in the table once, with the register's columns.
for (const registerRow of registerRows) {
  const row = byId.get(registerRow.id)
  if (!row) { say(`${REGISTER}:${registerRow.line}: ${registerRow.id}: no row in ${RULES}/`); continue }
  for (const [column, text] of Object.entries(automatic(registerRow))) {
    if (row[column] !== cell(text)) say(`${row.file}:${row.line}: ${registerRow.id}: ${column} differs from the register (${REGISTER}:${registerRow.line})`)
  }
}

// 2. Every row is a register row or a trace row, ids are unique, the words are
//    in the vocabulary.
const registerIds = new Set(registerRows.map(row => row.id))
const seen = new Set()
for (const row of rows) {
  if (seen.has(row.id)) say(`${row.file}:${row.line}: ${row.id}: a second row with this id`)
  seen.add(row.id)
  if (!registerIds.has(row.id) && !/^[A-Z]{2,6}(?:-[A-Z]{2,12})*-\d+$/.test(row.id)) {
    say(`${row.file}:${row.line}: ${row.id}: not a register row and not an id of the form TRC-…-1 or ECA-1`)
  }
  if (!STATUSES.includes(row.状态)) say(`${row.file}:${row.line}: ${row.id}: status ${row.状态 || '(empty)'} is not one of ${STATUSES.join(', ')}`)
  if (!LEVELS.includes(row.级别)) say(`${row.file}:${row.line}: ${row.id}: level ${row.级别 || '(empty)'} is not one of ${LEVELS.join(', ')}`)
}

// 3. The evidence resolves, every relative link of this directory resolves,
//    and the index is current.
for (const row of rows) evidence(row, counts)
for (const file of [INDEX, 'docs/formal-trace/README.en.md', ...FILES.map(([name]) => `${RULES}/${name}`), ...HANDWRITTEN.map(name => `${RULES}/${name}`)]) {
  if (exists(file)) for (const broken of links(file)) say(`broken link ${broken}`)
}
if (process.argv.includes('--write')) refreshIndex(rows)
else if (exists(INDEX)) {
  const block = indexBlock(read(INDEX))
  if (block !== null && block !== index(rows)) say(`${INDEX}: the index between the markers is stale; run node tools/formal-trace.mjs --write`)
}

if (!process.argv.includes('--check')) {
  const tally = list => {
    const counted = new Map()
    for (const row of list) counted.set(row.状态, (counted.get(row.状态) ?? 0) + 1)
    return STATUSES.filter(status => counted.has(status)).map(status => `${status} ${counted.get(status)}`).join(' · ')
  }
  console.log(`\nrows: ${rows.length} (register ${registerRows.length}, hand-written ${rows.length - registerRows.length})\n`)
  console.log('by file:')
  for (const file of [...FILES.map(([name]) => name), ...HANDWRITTEN]) {
    const list = rows.filter(row => row.file === `${RULES}/${file}`)
    if (list.length) console.log(`  ${file}: ${list.length} — ${tally(list)}`)
  }
  console.log(`\nby status: ${tally(rows)}`)
  console.log('\nby level:')
  for (const level of LEVELS) {
    const list = rows.filter(row => row.级别 === level)
    console.log(`  ${level} ${list.length}（${tally(list) || '—'}）`)
  }
  const unverified = rows.filter(row => row.级别 === '核心承诺' && row.状态 === '未验证').length
  const unverifiedRegister = registerRows.filter(row => row.level === '核心承诺' && status(row) === '未验证').length
  console.log(`\nunverified core commitments: ${unverified} (register rows: ${unverifiedRegister})`)
  const scoped = rows.filter(row => row.适用范围 !== '未登记').length
  console.log(`scope filled: ${scoped} / ${rows.length} (未登记 ${rows.length - scoped})`)
  console.log(`evidence: ${counts.references} references (names ${counts.names}, line ranges ${counts.ranges}); missing files ${counts.missing}, missing names ${counts.namesMissing}, lines beyond the file ${counts.rangesMissing}\n`)
}

const unique = [...new Set(problems)]
for (const line of unique) console.log(line)
console.log(`${unique.length} problem(s)`)
process.exitCode = unique.length ? 1 : 0
