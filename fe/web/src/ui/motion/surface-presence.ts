import { useEffectEvent, useLayoutEffect, useRef } from 'react';
import { playTrajectory, springTrajectory } from './spring.ts';
import { createPlaybackTarget } from './target.ts';
import { assertAxisAligned, frameCenter, surfaceKeyframe, rebaseSurface, validateFrame,
  type BindingView, type SurfaceBinding, type LiftSample, type Point, type SurfaceFrame, type SurfaceGeometry, type SurfaceMap, type SurfaceParts } from './geometry.ts';

export type SurfacePresenceInput = Readonly<{
  open: boolean; present: boolean;
  enabled: boolean; onExited: () => void;
  onVisualChange?: () => void;
  onMeasurementChange?: (view: BindingView) => void;
}> & (
  | Readonly<{ parts: SurfaceParts; geometry: Extract<SurfaceGeometry, { kind: 'lift' }> }>
  | Readonly<{ parts: Extract<SurfaceParts, { kind: 'paired' }>; geometry: Extract<SurfaceGeometry, { kind: 'edge-reveal' }> }>
);
type BoundPart = Readonly<{ element: HTMLElement; frame: SurfaceFrame }>;
type Snapshot = Readonly<{ parts: readonly BoundPart[]; origin: Point; map: SurfaceMap }>;
type Source = { element: HTMLElement | null; point: Point | null; frozen: boolean; center: boolean };

// One shared progress and fixed x/y compensation per declared part, all on one clock.
type SurfaceState = Readonly<{ progress: number; offsets: readonly Point[] }>;
type SurfaceSample = Readonly<{ value: SurfaceState; velocity: SurfaceState }>;
// Geometry, per-part sample and completion authority belong to the same binding.
// A suspended binding retains its sample, but has no target/completion authority.
type Binding = {
  snapshot: Snapshot; sample: SurfaceSample; target: number | null;
  playback: ReturnType<typeof createPlaybackTarget<SurfaceState>>;
};
function sameSnapshot(a: Snapshot, b: Snapshot) {
  return a.map.kind === b.map.kind
    && (a.map.kind !== 'edge-reveal' || b.map.kind === 'edge-reveal' && a.map.center.x === b.map.center.x && a.map.center.y === b.map.center.y)
    && a.parts.length === b.parts.length && a.parts.every((part, index) => {
    const next = b.parts[index]; const before = part.frame.viewportFromLocal; const after = next.frame.viewportFromLocal;
    return part.element === next.element && part.frame.document === next.frame.document
      && part.frame.size.width === next.frame.size.width && part.frame.size.height === next.frame.size.height
      && before.x === after.x && before.y === after.y && before.scaleX === after.scaleX && before.scaleY === after.scaleY;
  }) && a.origin.x === b.origin.x && a.origin.y === b.origin.y;
}
function transfer(from: SurfaceSample, old: Snapshot, next: Snapshot): SurfaceSample {
  const samples = next.parts.map((part, index) => {
    // Retained nodes take their own channels through reordering/cardinality changes.
    // An unmatched replacement inherits its old declaration slot only if that
    // element did not move elsewhere. A genuinely new part has no old contribution.
    const retained = old.parts.findIndex(before => before.element === part.element);
    const slot = old.parts[index];
    const previous = retained >= 0 ? retained : slot !== undefined
      && !next.parts.some(current => current.element === slot.element) ? index : -1;
    if (previous < 0) return { value: { progress: from.value.progress, x: 0, y: 0 },
      velocity: { progress: from.velocity.progress, x: 0, y: 0 } };
    return rebaseSurface(partSample(from, previous), old.parts[previous].frame, old.origin, old.map, part.frame, next.origin, next.map);
  });
  return { value: { progress: from.value.progress, offsets: samples.map(sample => ({ x: sample.value.x, y: sample.value.y })) },
    velocity: { progress: from.velocity.progress, offsets: samples.map(sample => ({ x: sample.velocity.x, y: sample.velocity.y })) } };
}
function rest(progress: number, count: number): SurfaceSample {
  return { value: { progress, offsets: Array.from({ length: count }, () => ({ x: 0, y: 0 })) },
    velocity: { progress: 0, offsets: Array.from({ length: count }, () => ({ x: 0, y: 0 })) } };
}
function partSample(sample: SurfaceSample, index: number): LiftSample {
  return { value: { progress: sample.value.progress, ...sample.value.offsets[index] },
    velocity: { progress: sample.velocity.progress, ...sample.velocity.offsets[index] } };
}
function playSurface(elements: readonly HTMLElement[], snapshot: Snapshot, from: SurfaceSample, target: number, onVisualChange?: () => void) {
  const p = springTrajectory(from.value.progress, target, from.velocity.progress);
  const offsets = from.value.offsets.map((point, index) => ({
    x: springTrajectory(point.x, 0, from.velocity.offsets[index].x),
    y: springTrajectory(point.y, 0, from.velocity.offsets[index].y),
  }));
  return playTrajectory<SurfaceState>(elements, {
    scaleVelocity: (value, rate) => ({ progress: value.progress * rate,
      offsets: value.offsets.map(point => ({ x: point.x * rate, y: point.y * rate })) }),
    duration: Math.max(p.duration, ...offsets.flatMap(({ x, y }) => [x.duration, y.duration])),
    sample: time => {
      const progress = p.sample(Math.min(time, p.duration));
      const samples = offsets.map(({ x, y }) => ({ x: x.sample(Math.min(time, x.duration)), y: y.sample(Math.min(time, y.duration)) }));
      return { value: { progress: progress.value, offsets: samples.map(({ x, y }) => ({ x: x.value, y: y.value })) },
        velocity: { progress: progress.velocity, offsets: samples.map(({ x, y }) => ({ x: x.velocity, y: y.velocity })) } };
    },
  }, (state, index) => surfaceKeyframe(snapshot.parts[index].frame, snapshot.origin, snapshot.map, { progress: state.progress, ...state.offsets[index] }), onVisualChange);
}

/** Paint retention only: never owns selection, navigation, focus or business disposal. */
export function useSurfacePresence(input: SurfacePresenceInput): SurfaceBinding {
  const visualChange = useEffectEvent(() => { input.onVisualChange?.(); });
  const latestParts = useRef(input.parts);
  const measurementView = useRef<BindingView>({ kind: 'unavailable', lifetime: {}, revision: 0 });
  const sameParts = (a: readonly BoundPart[], b: readonly BoundPart[]) => sameSnapshot({ parts: a, origin: { x: 0, y: 0 }, map: { kind: 'lift' } }, { parts: b, origin: { x: 0, y: 0 }, map: { kind: 'lift' } });
  const notify = useEffectEvent((ready: boolean) => {
    const current = binding.current;
    const before = measurementView.current;
    const parts = ready && current !== null ? current.snapshot.parts : null;
    if (parts === null ? before.kind === 'unavailable' : before.kind === 'ready' && sameParts(before.parts, parts)) return;
    measurementView.current = parts === null ? { kind: 'unavailable', lifetime: before.lifetime, revision: before.revision + 1 }
      : { kind: 'ready', lifetime: before.lifetime, revision: before.revision + 1, parts };
    input.onMeasurementChange?.(measurementView.current);
  });
  const primaryRef = input.parts.primary.element;
  const peerRef = input.parts.kind === 'paired' ? input.parts.peer.element : null;
  const binding = useRef<Binding | null>(null);
  const source = useRef<Source | null>(null);
  const exited = useRef(false);
  const cancel = () => binding.current?.playback.cancel();
  const suspend = () => {
    const owned = binding.current;
    if (owned === null) return;
    owned.sample = owned.playback.sample() ?? owned.sample;
    owned.playback.cancel(); owned.target = null;
  };
  const exit = useEffectEvent(() => {
    if (input.open || exited.current) return;
    exited.current = true; source.current = null; input.onExited();
  });
  const complete = useEffectEvent((current: Binding, target: number) => {
    if (binding.current !== current) return;
    current.sample = rest(target, current.snapshot.parts.length);
    // Read the latest declarations, even if their ref objects changed while the
    // actual elements stayed equal. Completion belongs to current paint nodes.
    const declarations = input.parts.kind === 'single' ? [input.parts.primary] : [input.parts.primary, input.parts.peer];
    if (declarations.length !== current.snapshot.parts.length || !declarations.every((part, index) => {
      const element = current.snapshot.parts[index].element;
      return part.element.current === element && element.isConnected;
    }) || declarations.some(part => part.readFrame() === null)) { current.target = null; notify(false); return; }
    if (target === 0) exit();
  });
  const update = useEffectEvent(() => {
    const { parts, geometry, open, present, enabled } = input;
    binding.current?.playback.setOnVisualChange(input.onVisualChange === undefined ? undefined : visualChange);
    latestParts.current = parts;
    let ready = false;
    try {
    if (!present) {
      cancel(); binding.current = null; source.current = null; return;
    }
    if (open) {
      if (exited.current) { cancel(); binding.current = null; }
      exited.current = false;
    } else if (exited.current) return;
    const declarations = parts.kind === 'single' ? [parts.primary] : [parts.primary, parts.peer];
    const elements = declarations.map(part => part.element.current);
    if (elements.some(element => element === null || !element.isConnected)) { suspend(); return; }
    const surfaces = elements as HTMLElement[];
    const document = surfaces[0].ownerDocument;
    const view = document.defaultView;
    try {
      if (surfaces.some(element => element.ownerDocument !== document)) throw new Error('Surface parts must share a document');
      if (new Set(surfaces).size !== surfaces.length) throw new Error('Surface parts must be distinct elements');
      const frames = declarations.map(part => part.readFrame());
      if (frames.some(frame => frame === null)) { suspend(); return; }
      const finalFrames = frames as SurfaceFrame[];
      for (const frame of finalFrames) validateFrame(frame, document);
      const frame = finalFrames[0];
      const origin = geometry.origin;
      let point: Point;
      if (origin.kind === 'paired-edge') {
        if (parts.kind !== 'paired') throw new Error('Paired edge requires paired parts');
        const end = (origin.edge === 'inline-end') !== (view!.getComputedStyle(surfaces[0]).direction === 'rtl');
        point = { x: frame.viewportFromLocal.x + (end ? frame.size.width * frame.viewportFromLocal.scaleX : 0), y: frameCenter(frame).y };
      } else if (origin.kind === 'anchor') {
        source.current ??= { element: origin.anchor.ref.current, point: null, frozen: false, center: false };
        const owned = source.current;
        const element = owned.element;
        if (element !== null && element.ownerDocument !== document) throw new Error('Surface source must share a document');
        if (!owned.frozen && !owned.center) {
          if (element === null || origin.anchor.ref.current !== element || !element.isConnected || element.getClientRects().length === 0
            || view!.getComputedStyle(element).visibility !== 'visible') owned.frozen = true;
          else {
            assertAxisAligned(element);
            const rect = element.getBoundingClientRect();
            if (rect.width > 0 && rect.height > 0) owned.point = { x: rect.left + rect.width / 2, y: rect.top + rect.height / 2 };
            else owned.frozen = true;
          }
          // The explicit fallback is a whole-cycle decision, never a later ref acquisition.
          if (owned.point === null) owned.center = true;
        }
        point = owned.point ?? frameCenter(frame);
      } else point = frameCenter(frame);
      const next: Snapshot = { parts: surfaces.map((element, index) => ({ element, frame: finalFrames[index] })), origin: point,
        map: geometry.kind === 'edge-reveal' ? { kind: 'edge-reveal', center: frameCenter(frame) } : { kind: 'lift' } };
      const target = open ? 1 : 0;
      ready = true;
      const owned = binding.current;
      const changed = owned === null || !sameSnapshot(owned.snapshot, next);
      if (view === null || !enabled || view.matchMedia('(prefers-reduced-motion: reduce)').matches) {
        const moving = owned?.playback.moving() ?? false;
        cancel(); binding.current = { snapshot: next, target, sample: rest(target, surfaces.length), playback: createPlaybackTarget<SurfaceState>() };
        if (moving) visualChange(); if (!open) exit(); return;
      }
      if (!changed && owned.target === target) return;
      // Revoke the entire old group's authority before mapping or acquiring effects.
      const sampled = owned === null ? rest(0, surfaces.length) : owned.playback.sample() ?? owned.sample;
      cancel(); binding.current = null;
      const from = owned === null ? sampled : transfer(sampled, owned.snapshot, next);
      const current: Binding = { snapshot: next, target, sample: from, playback: createPlaybackTarget<SurfaceState>() };
      binding.current = current;
      if (from.value.progress === target && from.velocity.progress === 0
        && [...from.value.offsets, ...from.velocity.offsets].every(point => point.x === 0 && point.y === 0)) { if (!open) exit(); return; }
      const active = playSurface(surfaces, next, from, target, input.onVisualChange === undefined ? undefined : visualChange);
      current.playback.replace(active, () => { complete(current, target); });
    } catch (error) {
      ready = false; cancel(); binding.current = null;
      throw error;
    }
    } finally { notify(ready); }
  });
  // Lifetime setup precedes acquisition; cleanup never notifies an unmounted owner.
  useLayoutEffect(() => {
    measurementView.current = { kind: 'unavailable', lifetime: {}, revision: 0 };
    return () => { measurementView.current = { kind: 'unavailable', lifetime: {}, revision: 0 }; };
  }, []);
  // A layout owner may change its final frame during any committed render.
  useLayoutEffect(() => { update(); });
  const observing = useRef<{ nodes: readonly (HTMLElement | null)[]; dispose: () => void } | null>(null);
  useLayoutEffect(() => {
    const nodes = input.present ? [primaryRef.current, peerRef?.current ?? null, source.current?.element ?? null] : [];
    if (observing.current !== null && observing.current.nodes.length === nodes.length
      && observing.current.nodes.every((node, index) => node === nodes[index])) return;
    observing.current?.dispose(); observing.current = null;
    if (!input.present) return;
    const element = primaryRef.current;
    const document = element?.ownerDocument; const view = document?.defaultView;
    if (document == null || view == null) return;
    let queued: number | null = null;
    const lifetime = measurementView.current.lifetime;
    const current = () => measurementView.current.lifetime === lifetime && observing.current?.nodes === nodes;
    const schedule = () => {
      if (!current() || queued !== null) return;
      queued = view.requestAnimationFrame(() => { queued = null; if (current()) update(); });
    };
    const preference = view.matchMedia('(prefers-reduced-motion: reduce)');
    const reduce = () => { if (current()) update(); };
    const observer = new ResizeObserver(schedule);
    if (element != null) observer.observe(element);
    if (peerRef?.current != null) observer.observe(peerRef.current);
    const anchor = source.current?.element;
    if (anchor != null) observer.observe(anchor);
    view.addEventListener('resize', schedule);
    document.addEventListener('scroll', schedule, true);
    view.visualViewport?.addEventListener('resize', schedule);
    view.visualViewport?.addEventListener('scroll', schedule);
    preference.addEventListener('change', reduce);
    observing.current = { nodes, dispose: () => {
      if (queued !== null) view.cancelAnimationFrame(queued);
      observer.disconnect(); view.removeEventListener('resize', schedule);
      document.removeEventListener('scroll', schedule, true);
      view.visualViewport?.removeEventListener('resize', schedule);
      view.visualViewport?.removeEventListener('scroll', schedule);
      preference.removeEventListener('change', reduce);
    } };
  });
  useLayoutEffect(() => () => { observing.current?.dispose(); observing.current = null; cancel(); source.current = null; binding.current = null; }, []);
  const readBinding = () => {
    const current = measurementView.current;
    if (current.kind === 'unavailable') return current;
    const parts = latestParts.current;
    const declarations = parts.kind === 'single' ? [parts.primary] : [parts.primary, parts.peer];
    const live = declarations.map(part => ({ element: part.element.current, frame: part.readFrame() }));
    if (live.length !== current.parts.length || live.some(part => part.element === null || !part.element.isConnected || part.frame === null)
      || !sameParts(current.parts, live as BoundPart[])) return { kind: 'unavailable' as const, lifetime: current.lifetime, revision: current.revision };
    return current;
  };
  const measurement = useRef<SurfaceBinding>({ readBinding });
  return measurement.current;
}
