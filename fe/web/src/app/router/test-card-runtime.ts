// A booted card runtime for router tests.
//
// `AppRouterDeps.cards` is required, not optional: a router assembled without a
// card registry would silently list every kernel card, including the headless
// ones, so "forgot to wire it" must be a compile error rather than a fallback.
// This helper keeps that requirement from costing every router test five lines.
// It is deliberately the real registry with the real built-ins — a stub here
// would let the track route's filtering drift from production.

import { createCardHost, createCardRegistry } from '../../systems/cards/public.js';
import { bootCards } from '../cards.ts';
import type { CardRuntime } from './public.tsx';

export function bootTestCardRuntime(): CardRuntime {
  const registry = createCardRegistry();
  bootCards(registry);
  return { registry, host: createCardHost(registry) };
}
