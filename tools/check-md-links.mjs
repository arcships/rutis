#!/usr/bin/env node
// Relative links in the repository's Markdown files point at files that
// exist. Offline: external links (http, mailto) and anchors are not checked.
// Used by ci.yml for pull requests that change documentation.
//
//   node tools/check-md-links.mjs              report every broken link
//   node tools/check-md-links.mjs --base REV   fail only on links broken now
//                                              and not already broken at REV
//
// Older design and review notes link to paths that have since moved; with
// --base those are listed as known and do not fail the check, so a change
// fails only for the links it breaks (adds, or breaks by moving a file).

import { execFileSync } from 'node:child_process';
import { existsSync, readFileSync } from 'node:fs';
import { posix } from 'node:path';

const git = (...args) => execFileSync('git', args, { encoding: 'utf8', maxBuffer: 1 << 28 });
const root = git('rev-parse', '--show-toplevel').trim();
const baseIndex = process.argv.indexOf('--base');
const base = baseIndex > 0 ? process.argv[baseIndex + 1] : null;

// [text](target "title"), ![alt](target), and [label]: target definitions.
const inline = /!?\[(?:[^\]\\]|\\.)*\]\(\s*(<[^>]*>|[^\s)]+)(?:\s+(?:"[^"]*"|'[^']*'))?\s*\)/g;
const definition = /^\s{0,3}\[[^\]]+\]:\s*(<[^>]*>|\S+)/;

// The relative link targets of one Markdown text, with their line numbers.
function links(text) {
  const found = [];
  let fence = null;
  text.split('\n').forEach((line, index) => {
    const marker = line.match(/^\s*(`{3,}|~{3,})/);
    if (marker) {
      if (!fence) fence = marker[1];
      else if (marker[1][0] === fence[0] && marker[1].length >= fence.length) fence = null;
      return;
    }
    if (fence) return;
    const code = line.replace(/`[^`]*`/g, '');
    const targets = [...code.matchAll(inline)].map((m) => m[1]);
    const def = code.match(definition);
    if (def) targets.push(def[1]);
    for (let target of targets) {
      target = target.replace(/^<|>$/g, '');
      if (/^[a-z][a-z0-9+.-]*:/i.test(target) || target.startsWith('#') || target.startsWith('//')) continue;
      if (target.replace(/[#?].*$/, '')) found.push({ target, line: index + 1 });
    }
  });
  return found;
}

// The repository path a link in `file` points at.
function resolveLink(file, target) {
  let bare = target.replace(/[#?].*$/, '');
  try { bare = decodeURIComponent(bare); } catch { /* keep as written */ }
  return posix.normalize(bare.startsWith('/') ? bare.slice(1) : posix.join(posix.dirname(file), bare))
    .replace(/\/$/, '');
}

// Broken links of a tree, as "file -> target" keys.
function broken({ files, read, exists }) {
  const result = new Map();
  for (const file of files) {
    for (const { target, line } of links(read(file))) {
      if (!exists(resolveLink(file, target))) result.set(`${file} -> ${target}`, `${file}:${line}: ${target}`);
    }
  }
  return result;
}

const head = broken({
  files: git('-C', root, 'ls-files', '-z', '*.md').split('\0').filter(Boolean)
    .filter((file) => existsSync(posix.join(root, file))),
  read: (file) => readFileSync(posix.join(root, file), 'utf8'),
  exists: (path) => path === '.' || existsSync(posix.join(root, path)),
});

let known = new Map();
if (base) {
  const tree = git('-C', root, 'ls-tree', '-r', '-t', '--name-only', '-z', base).split('\0').filter(Boolean);
  const paths = new Set(tree);
  known = broken({
    files: tree.filter((file) => file.endsWith('.md')),
    read: (file) => git('-C', root, 'show', `${base}:${file}`),
    exists: (path) => path === '.' || paths.has(path),
  });
}

const added = [...head].filter(([key]) => !known.has(key));
const kept = head.size - added.length;
if (kept > 0) console.log(`${kept} broken link(s) already present at ${base}, not checked`);
for (const [, where] of added) console.log(`broken link ${where}`);
if (added.length > 0) {
  console.log(`${added.length} broken relative link(s)`);
  process.exit(1);
}
console.log('relative links resolve');
