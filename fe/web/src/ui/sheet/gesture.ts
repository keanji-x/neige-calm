import type { SheetMotion } from './motion.ts';

type Pointer = Pick<PointerEvent, 'pointerId' | 'isPrimary' | 'button' | 'clientY' | 'timeStamp' | 'preventDefault'>;
type Lease = { id: number; handle: HTMLElement; origin: number; rawBase: number; minimum: number; lastY: number; lastTime: number; travel: number; raw: number; velocity: number };

/** Handle-only authority. Clear the opaque lease before releasing capture:
 * lostpointercapture and all stale end events then have no authority. */
export function createSheetGesture(motion: SheetMotion, available: () => boolean, height: () => number, settle: () => void, dismiss: () => void) {
  let lease: Lease | null = null;
  const clear = () => {
    const old = lease; lease = null;
    if (old?.handle.hasPointerCapture(old.id)) old.handle.releasePointerCapture(old.id);
    return old;
  };
  const end = (cancelled: boolean) => {
    const old = clear();
    if (old === null) return;
    if (!cancelled && old.travel !== 0 && (Math.abs(old.velocity) > 1.2 && Math.abs(old.travel) > 48 ? old.travel > 0 : old.raw > height() * 0.4)) dismiss();
    else settle();
  };
  return {
    down: (event: Pointer, handle: HTMLElement) => {
      if (!available() || lease !== null || !event.isPrimary || event.button !== 0) return;
      event.preventDefault();
      const sample = motion.hold();
      // The spring may already paint beyond the finger's -48 cap. Preserve
      // only that acquired overshoot, without allowing the finger to deepen it.
      const rawBase = sample.value < 0 ? sample.value / 0.35 : sample.value;
      lease = { id: event.pointerId, handle, origin: event.clientY, rawBase, minimum: Math.min(-48, sample.value), lastY: event.clientY,
        lastTime: event.timeStamp, travel: 0, raw: rawBase, velocity: 0 };
      handle.setPointerCapture(event.pointerId);
    },
    move: (event: Pointer) => {
      const old = lease;
      if (old === null || old.id !== event.pointerId || !available()) return;
      const dt = event.timeStamp - old.lastTime;
      if (dt <= 0 || event.clientY === old.lastY) return;
      const velocity = (event.clientY - old.lastY) / dt;
      old.travel = event.clientY - old.origin; old.raw = old.rawBase + old.travel;
      old.velocity = velocity; old.lastTime = event.timeStamp; old.lastY = event.clientY;
      // Retire the acquisition exception on re-entering the ordinary domain;
      // reversal must then obey the original cap for the rest of this lease.
      if (old.raw >= -48 / 0.35) old.minimum = -48;
      const value = old.raw < 0 ? Math.max(old.minimum, old.raw * 0.35) : old.raw;
      const derivative = old.raw < old.minimum / 0.35 || old.raw === old.minimum / 0.35 && velocity < 0 ? 0
        : old.raw < 0 || old.raw === 0 && velocity < 0 ? 0.35 : 1;
      motion.direct({ value, velocity: derivative === 0 ? 0 : velocity * derivative * 1000 });
    },
    up: (event: Pointer) => { if (lease?.id === event.pointerId) end(false); },
    cancelPointer: (event: Pick<Pointer, 'pointerId'>) => { if (lease?.id === event.pointerId) end(true); },
    cancel: () => end(true),
    revoke: () => { clear(); },
    dragging: () => lease !== null,
  };
}
