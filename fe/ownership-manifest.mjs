import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { parse } from 'yaml';
import { duplicationManifest } from './tools/architecture/duplication-manifest.mjs';

// The stage-1 inventory is the input; this manifest is the formal generated
// exit. Keeping generation here prevents ownership from becoming an alternate
// path-design source during stage 2.
const inventory = parse(readFileSync(resolve(import.meta.dirname, 'module-file-inventory.yaml'), 'utf8'));

for (const duplication of duplicationManifest) {
  const canonicalPath = `fe/${duplication.canonicalPath}`;
  const references = inventory.filter(({ basis }) => typeof basis === 'string' && basis.includes(duplication.id));
  const coversCanonical = references.some(({ path, type }) => type === 'file'
    ? path === canonicalPath : canonicalPath === path || canonicalPath.startsWith(`${path}/`));
  if (!coversCanonical) throw new Error(`${duplication.id} basis must cover canonical path ${canonicalPath}`);
}

export const ownershipManifest = Object.freeze(inventory.map(({ path, type, owner, readonly, basis }) => Object.freeze({
  path, type, owner, readonly, basis,
})));
