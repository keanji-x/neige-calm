import { ok } from 'node:assert/strict';
import { readFileSync } from 'node:fs';

/** @param {string} source */
export function assertSourceRemainsGuarded(source) {
  ok(readFileSync(source, 'utf8').includes('export const guarded = true;'), 'source remains guarded');
}
