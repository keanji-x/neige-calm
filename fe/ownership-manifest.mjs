import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { parse } from 'yaml';

// The stage-1 inventory is the input; this manifest is the formal generated
// exit. Keeping generation here prevents ownership from becoming an alternate
// path-design source during stage 2.
const inventory = parse(readFileSync(resolve(import.meta.dirname, 'module-file-inventory.yaml'), 'utf8'));

export const ownershipManifest = Object.freeze(inventory.map(({ path, type, owner, readonly }) => Object.freeze({
  path, type, owner, readonly,
})));
