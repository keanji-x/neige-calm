import { useEffectEvent, useLayoutEffect, useRef } from 'react';
import { validateFrame, type SurfaceFrame, type SurfacePart, type SurfaceSource } from './geometry.ts';
import { playTrajectory, springTrajectory, type SpringSample } from './spring.ts';
import { createPlaybackTarget } from './target.ts';

export type EdgePresenceInput = Readonly<{
  part: SurfacePart;
  frame: SurfaceSource;
  edge: 'inline-start' | 'inline-end' | 'bottom';
  open: boolean;
  present: boolean;
  enabled: boolean;
  onExited: () => void;
}>;
type Snapshot = Readonly<{ element: HTMLElement; frame: SurfaceFrame; vertical: boolean; distance: number }>;
type Binding = {
  snapshot: Snapshot;
  sample: SpringSample;
  target: number | null;
  playback: ReturnType<typeof createPlaybackTarget<number>>;
};
function same(a: Snapshot, b: Snapshot) {
  return a.element === b.element && a.vertical === b.vertical && a.distance === b.distance
    && a.frame.document === b.frame.document
    && a.frame.viewportFromLocal.scaleX === b.frame.viewportFromLocal.scaleX
    && a.frame.viewportFromLocal.scaleY === b.frame.viewportFromLocal.scaleY;
}

/** One viewport CSS-pixel contribution and signed px/s, independent of layout motion.
 * The caller supplies an unanimated final frame and a separate, exclusive paint node.
 * Retention is paint only; semantic permissions and focus remain with the caller.
 */
export function useEdgePresence(input: EdgePresenceInput): void {
  const binding = useRef<Binding | null>(null);
  const exited = useRef(false);
  const cancel = () => { binding.current?.playback.cancel(); };
  const suspend = () => {
    const owned = binding.current;
    if (owned === null) return;
    owned.sample = owned.playback.sample() ?? owned.sample;
    owned.playback.cancel(); owned.target = null;
  };
  const exit = useEffectEvent(() => {
    if (input.open || !input.present || exited.current) return;
    exited.current = true; input.onExited();
  });
  const complete = useEffectEvent((current: Binding, target: number) => {
    if (binding.current !== current) return;
    current.sample = { value: target, velocity: 0 };
    const element = current.snapshot.element;
    try {
      const positioner = input.frame.ref.current;
      if (input.part.element.current !== element || !element.isConnected
        || positioner === null || !positioner.isConnected) { suspend(); return; }
      const frame = input.part.readFrame();
      if (frame === null) { suspend(); return; }
      validateFrame(frame, element.ownerDocument);
      validateFrame(frame, positioner.ownerDocument);
      // An adopted node cannot grant an old document's completion before commit.
      if (frame.document !== current.snapshot.frame.document) { suspend(); return; }
    } catch (error) {
      cancel(); binding.current = null;
      throw error;
    }
    if (target !== 0) exit();
  });
  const update = useEffectEvent(() => {
    const { part, open, present, enabled, edge } = input;
    if (!present) { cancel(); binding.current = null; exited.current = false; return; }
    if (open) exited.current = false;
    else if (exited.current) return;
    const element = part.element.current;
    const positioner = input.frame.ref.current;
    if (element === null || !element.isConnected || positioner === null || !positioner.isConnected) { suspend(); return; }
    try {
    const frame = part.readFrame();
    if (frame === null) { suspend(); return; }
    validateFrame(frame, element.ownerDocument);
    validateFrame(frame, positioner.ownerDocument);
    const view = element.ownerDocument.defaultView!;
    const vertical = edge === 'bottom';
    const end = (edge === 'inline-end') !== (view.getComputedStyle(element).direction === 'rtl');
    const distance = vertical ? frame.size.height * frame.viewportFromLocal.scaleY
      : frame.size.width * frame.viewportFromLocal.scaleX * (end ? 1 : -1);
    const next: Snapshot = { element, frame, vertical, distance };
    const target = open ? 0 : distance;
    const owned = binding.current;
    if (!enabled || view.matchMedia('(prefers-reduced-motion: reduce)').matches) {
      cancel(); binding.current = { snapshot: next, target, sample: { value: target, velocity: 0 }, playback: createPlaybackTarget<number>() };
      if (!open) exit();
      return;
    }
    if (owned !== null && same(owned.snapshot, next) && owned.target === target) return;
    // Sample the old native clock BEFORE revoking its effect. These are already
    // viewport contributions / wall-clock derivatives, so resize adds no ratio.
    const from = owned === null ? { value: distance, velocity: 0 } : owned.playback.sample() ?? owned.sample;
    cancel();
    const current: Binding = { snapshot: next, sample: from, target, playback: createPlaybackTarget<number>() };
    binding.current = current;
    if (from.value === target && from.velocity === 0) { if (!open) exit(); return; }
    const trajectory = springTrajectory(from.value, target, from.velocity);
    const scale = vertical ? frame.viewportFromLocal.scaleY : frame.viewportFromLocal.scaleX;
    const playback = playTrajectory([element], { ...trajectory, scaleVelocity: (velocity, rate) => velocity * rate }, offset => ({
      transform: vertical ? `translateY(${offset / scale}px)` : `translateX(${offset / scale}px)`,
    }));
    current.playback.replace(playback, () => { complete(current, target); });
    } catch (error) {
      cancel(); binding.current = null;
      throw error;
    }
  });
  useLayoutEffect(() => { update(); });
  const observing = useRef<{
    element: HTMLElement | null; positioner: HTMLElement | null;
    document: Document; view: Window; frameDocument: Document | null; frameView: Window | null;
    paintRef: SurfacePart['element']; frameRef: SurfaceSource['ref']; dispose: () => void;
  } | null>(null);
  useLayoutEffect(() => {
    const element = input.present ? input.part.element.current : null;
    const positioner = input.present ? input.frame.ref.current : null;
    const paintRef = input.part.element; const frameRef = input.frame.ref;
    const host = element ?? positioner;
    const document = host?.ownerDocument; const view = document?.defaultView;
    const frameDocument = positioner?.ownerDocument ?? null; const frameView = frameDocument?.defaultView ?? null;
    if (observing.current?.element === element && observing.current.positioner === positioner
      && observing.current.paintRef === paintRef && observing.current.frameRef === frameRef
      && observing.current.document === document && observing.current.view === view
      && observing.current.frameDocument === frameDocument && observing.current.frameView === frameView) return;
    observing.current?.dispose(); observing.current = null;
    if (document === undefined || view == null) return;
    let queued: number | null = null;
    const record = { element, positioner, document, view, frameDocument, frameView, paintRef, frameRef, dispose: () => {
      if (queued !== null) view.cancelAnimationFrame(queued);
      observer.disconnect(); view.removeEventListener('resize', schedule);
      document.removeEventListener('scroll', schedule, true);
      view.visualViewport?.removeEventListener('resize', schedule);
      view.visualViewport?.removeEventListener('scroll', schedule);
      preference.removeEventListener('change', reduce);
    } };
    const schedule = () => {
      if (observing.current !== record || queued !== null) return;
      queued = view.requestAnimationFrame(() => { queued = null; if (observing.current === record) update(); });
    };
    const reduce = () => { if (observing.current === record) update(); };
    const observer = new ResizeObserver(schedule);
    if (element !== null) observer.observe(element);
    if (positioner !== null && positioner !== element) observer.observe(positioner);
    const preference = view.matchMedia('(prefers-reduced-motion: reduce)');
    view.addEventListener('resize', schedule);
    document.addEventListener('scroll', schedule, true);
    view.visualViewport?.addEventListener('resize', schedule);
    view.visualViewport?.addEventListener('scroll', schedule);
    preference.addEventListener('change', reduce);
    observing.current = record;
  });
  useLayoutEffect(() => () => {
    observing.current?.dispose(); observing.current = null;
    cancel(); binding.current = null; exited.current = false;
  }, []);
}
