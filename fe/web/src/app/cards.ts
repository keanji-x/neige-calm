import type { CardRegistry } from '../systems/cards/public.js';
import { registerAvailableBuiltinCards } from '../systems/cards/public.js';

/** The app's one call into the card system at boot; the built-in set and order are owned by `systems/cards/builtins`. */
export function bootCards(registry: CardRegistry): void {
  registerAvailableBuiltinCards(registry);
}
