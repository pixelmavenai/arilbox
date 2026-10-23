// Checks web/src/i18n/en.json against the code: every literal t('key') / tOr('key', …) must
// exist, and every entry must be used, except families looked up with a computed key.
import { readFileSync, readdirSync, statSync } from 'node:fs';
import { join } from 'node:path';

const root = new URL('../src/', import.meta.url).pathname;
const messages = JSON.parse(readFileSync(join(root, 'i18n/en.json'), 'utf8'));

// Looked up as `prefix + value` (status names, step ids, error codes, …).
const dynamicPrefixes = [
  'status.',
  'step.',
  'error.',
  'job.kind.',
  'template.',
  'action.start',
  'action.stop',
  'action.restart',
  'container.tab.',
  'console.',
  'settings.theme.',
  'nav.',
];

function* files(dir) {
  for (const name of readdirSync(dir)) {
    const path = join(dir, name);
    if (statSync(path).isDirectory()) yield* files(path);
    else if (/\.(ts|svelte)$/.test(name)) yield path;
  }
}

const used = new Set();
const pattern = /\b(?:t|tOr|has)\(\s*'([^']+)'/g;
const literal = /'((?:[a-z0-9_]+\.)+[a-z0-9_]+)'/g;
for (const file of files(root)) {
  const text = readFileSync(file, 'utf8');
  for (const m of text.matchAll(pattern)) used.add(m[1]);
  // Keys kept in tables (e.g. nav items, page titles) are quoted dotted strings.
  for (const m of text.matchAll(literal)) if (m[1] in messages) used.add(m[1]);
}

let failed = false;
for (const key of [...used].sort()) {
  if (!(key in messages)) {
    console.error(`missing in en.json: ${key}`);
    failed = true;
  }
}
for (const key of Object.keys(messages)) {
  if (!used.has(key) && !dynamicPrefixes.some((p) => key.startsWith(p))) {
    console.error(`unused in en.json: ${key}`);
    failed = true;
  }
}
if (failed) process.exit(1);
console.log(`i18n: ${Object.keys(messages).length} keys, ${used.size} used literally`);
