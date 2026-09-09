// Consume normalized output from an existing quote connector. This script does
// not log in, read credentials, fetch market data, or execute brokerage orders.
import { readFile, open, rename, unlink } from 'node:fs/promises';
import { resolve } from 'node:path';
import { applyQuoteUpdates, portfolioSnapshotSchema } from '../src/portfolio-framework.ts';

const [input, target = 'src/portfolio-snapshot.json', ...extra] = process.argv.slice(2);
if (!input || extra.length) throw new Error('Usage: update-quotes <quotes.json> [snapshot.json]');
const updates = JSON.parse(await readFile(resolve(input), 'utf8'));
if (!Array.isArray(updates)) throw new Error('Quote updates must be an array');
const path = resolve(target);
const lock = `${path}.lock`;
const temporary = `${path}.${process.pid}.tmp`;
const handle = await open(lock, 'wx', 0o600);
try {
  const previous = portfolioSnapshotSchema.parse(JSON.parse(await readFile(path, 'utf8')));
  const next = applyQuoteUpdates(previous, updates);
  const output = await open(temporary, 'wx', 0o600);
  try { await output.writeFile(`${JSON.stringify(next, null, 2)}\n`); }
  finally { await output.close(); }
  await rename(temporary, path);
  console.log(`Updated ${updates.length} quotes in ${path}`);
} finally {
  try { await unlink(temporary).catch(error => { if (error.code !== 'ENOENT') throw error; }); }
  finally { await handle.close(); await unlink(lock); }
}
