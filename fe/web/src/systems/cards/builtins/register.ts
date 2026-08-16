// Built-in card composition (`INV-CARD-225`).
//
// `BUILTIN_CARD_ORDER` is the single authority on registration order. The
// registry's kernel resolution falls back to a full scan in insertion order, so
// the order is business semantics (codex must get first refusal on kind
// `'codex'` before spec picks the card up), not an incidental detail.
//
// Entries that have not landed yet are simply **absent** from the registrar
// map and skipped. There are no placeholder, no-op or "unknown" entries: a card
// type either has a real adapter or the kernel card falls through to the
// unknown slot, which is a diagnosable state. Later slices add their real entry
// to this same map; nobody writes a second registration sequence.

import type { CardRegistry } from '../registry.js';
import { SPEC_CARD_ENTRY } from './spec.js';
import { WAVE_REPORT_CARD_ENTRY } from './wave-report.js';

export const BUILTIN_CARD_ORDER = Object.freeze([
  'terminal',
  'codex',
  'spec',
  'claude',
  'wave-report',
  'file-viewer',
  'iframe',
  'plugin-iframe',
] as const);

export type BuiltinCardType = (typeof BUILTIN_CARD_ORDER)[number];

/**
 * Register every built-in card that exists today, in `BUILTIN_CARD_ORDER`.
 *
 * The signature is fixed: the caller owns the registry instance and nothing
 * else. Entries are not injected — they belong to `systems/cards`, so app has
 * no way to reorder them or slip a different adapter in.
 */
export function registerAvailableBuiltinCards(registry: CardRegistry): void {
  // Keyed by `BuiltinCardType` and ordered like the tuple purely for reading;
  // the loop below, not this literal, decides the registration order. Each
  // value registers its own entry so the registry's per-card generic keeps the
  // narrow card type instead of widening to the whole `RegisteredCard` union.
  const registrars: Partial<Record<BuiltinCardType, (target: CardRegistry) => void>> = {
    spec: (target) => { target.register(SPEC_CARD_ENTRY); },
    'wave-report': (target) => { target.register(WAVE_REPORT_CARD_ENTRY); },
  };
  for (const type of BUILTIN_CARD_ORDER) {
    registrars[type]?.(registry);
  }
}
