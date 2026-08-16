import type { CardRegistry } from '../systems/cards/public.js';
import { registerAvailableBuiltinCards } from '../systems/cards/public.js';

/**
 * The app's one call into the card system at boot.
 *
 * It takes no entries, holds no order table and keeps no module state: the
 * order and the set of built-ins are owned by `systems/cards/builtins`, and the
 * registry instance is owned by whoever calls this. Everything this wrapper
 * still exists for is naming the moment in app boot when cards become
 * available.
 */
export function bootCards(registry: CardRegistry): void {
  registerAvailableBuiltinCards(registry);
}
