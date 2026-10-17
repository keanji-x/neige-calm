import type { RefObject } from 'react';
import { useSpringPresence } from '../../../ui/motion/presence.ts';

/** One local outgoing surface per pane, never an animation lifecycle per stored row. */
export function useSendMotion(ref: RefObject<HTMLElement | null>, sending: boolean) {
  useSpringPresence(ref, null, true, sending, true, () => {}, value => ({
    opacity: value, transform: `translateY(calc(var(--space-2) * ${1 - value}))`,
  }));
}
