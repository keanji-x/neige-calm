// A booted card runtime for router tests: the real registry with the real built-ins.

import { createCardHost, createCardRegistry } from '../../systems/cards/public.js';
import { bootCards } from '../cards.ts';
import type { CardRuntime } from './public.tsx';

export function bootTestCardRuntime(): CardRuntime {
  const registry = createCardRegistry();
  bootCards(registry);
  return { registry, host: createCardHost(registry) };
}
