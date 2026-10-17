import { useSpringArrival } from '../../../ui/motion/arrival.ts';

function paintArrival(value: number): Keyframe {
  return { opacity: value, transform: `translateY(calc(var(--space-2) * ${1 - value}))` };
}

/** One local outgoing surface per pane, never an animation lifecycle per stored row. */
export function useSendMotion() {
  return useSpringArrival(paintArrival);
}
