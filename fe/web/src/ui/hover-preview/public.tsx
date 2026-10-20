export { PreviewSummary, PreviewTextLink } from './summary.tsx';
import { useCallback, useEffect, useEffectEvent, useId, useLayoutEffect, useRef, type ReactNode } from 'react';
import { createPortal } from 'react-dom';
import { isImeKeyEvent } from '@astryxdesign/core/utils';
import { LayerDepthProvider, useLayer, useLayerDismissal } from '@astryxdesign/core/Layer';

import { useSurfacePresence } from '../motion/surface-presence.ts';
import { readSurfaceFrame, type BindingView } from '../motion/geometry.ts';
import { useState } from '../state/public.ts';
import { restoreDrawerFocus } from '../drawer/restore-focus.ts';
import { inPreviewBridge, placePreview, PREVIEW_GAP, PREVIEW_WIDTH, PREVIEW_MAX_HEIGHT, type Placement } from './placement.ts';
import styles from './preview.module.css';
import { CardFrame } from '../card-frame/public.tsx';
import { useInteractionScope } from './interaction-scope.ts';

type Phase = 'closed' | 'waiting' | 'open';
const HOVER_DELAY = 300;
const LEAVE_DELAY = 180;
const TRAVEL_DELAY = 800;
const NAVIGATION_DISMISS = 'nc-preview-navigation';
const INTERACTION_REGISTER = 'nc-preview-register';
type ParentView = { kind: 'pending' | 'revoked' } | { kind: 'admitted'; token: object; read: () => boolean };
type NestedInteraction = { setParent: (view: ParentView) => void; release: (() => void) | null };
type Paint = { kind: 'empty' } | { kind: 'suspended' | 'retained'; actual: HTMLDivElement; part: HTMLDivElement }
  | { kind: 'admitted'; actual: HTMLDivElement; part: HTMLDivElement; binding: Extract<BindingView, { kind: 'ready' }>; cycle: object; ancestor: object; slot: Placement; height: number };

// Render initializes new DOM safely, before child autofocus/layout work.
// The post-bind gate is the sole dynamic writer of public DOM permissions.
function gate(slot: HTMLElement | null, actual: HTMLElement | null, part: HTMLElement | null, admitted: boolean, paint: boolean) {
  const attribute = (element: HTMLElement, name: string, value: string | null) => {
    if (element.getAttribute(name) === value) return;
    if (value === null) element.removeAttribute(name); else element.setAttribute(name, value);
  };
  for (const element of [slot, part]) if (element !== null) {
    if (element.inert !== !admitted) element.inert = !admitted;
    attribute(element, 'aria-hidden', admitted ? null : 'true');
  }
  if (part !== null) for (const name of ['data-nc-link-preview', 'data-nc-escape-layer']) attribute(part, name, admitted ? '' : null);
  const clip = paint ? 'none' : 'inset(50%)';
  if (actual !== null && actual.style.clipPath !== clip) actual.style.clipPath = clip;
}

/** Transient, non-modal preview. The host owns destination admission and content.
 * Timers, portal and listeners live only as long as this trigger.
 * ArrowDown moves focus into the preview content; Escape closes the topmost preview. No focus trap.
 * Clicking a trigger still follows the host's ordinary navigation contract.
 */
export function HoverPreview({ title, trigger, children, getReadingSurface, getAvoidSurfaces }: Readonly<{
  title: string;
  trigger: (activate: () => void, dismissForNavigation: () => void) => ReactNode;
  /** Navigation dismisses this preview and its ancestors; ordinary close/Escape stays local. */
  children: ReactNode | ((dismissForNavigation: () => void) => ReactNode);
  /** Host-owned reading area. The primitive never infers document/application layout. */
  getReadingSurface?: (trigger: HTMLElement) => HTMLElement | null;
  getAvoidSurfaces?: (trigger: HTMLElement) => readonly HTMLElement[];
}>) {
  const parent = useRef<ParentView>({ kind: 'admitted', token: {}, read: () => true });
  const paint = useRef<Paint>({ kind: 'empty' });
  const cycle = useRef<object>({});
  const intent = useRef<Phase>('closed');
  const [, refresh] = useState(0);
  const schedule = useCallback(() => refresh(value => value + 1), [refresh]);
  const nested = useRef(new Set<NestedInteraction>());
  const inform = (view: ParentView) => { for (const child of nested.current) child.setParent(view); };
  const id = useId();
  const anchor = useRef<HTMLSpanElement>(null);
  const card = useRef<HTMLDivElement>(null);
  const actual = useRef<HTMLDivElement>(null);
  // A committed paint node is the retention capability, not merely open intent.
  const [candidate, setCandidate] = useState<HTMLDivElement | null>(null);
  const header = useRef<HTMLDivElement>(null);
  const intrinsic = useRef<HTMLDivElement>(null);
  const [actualHeight, setActualHeight] = useState(PREVIEW_MAX_HEIGHT);
  const positioner = useRef<HTMLDivElement>(null);
  const body = useRef<HTMLDivElement>(null);
  const focusOnOpen = useRef(false);
  const skipFocus = useRef(false);
  const focusNeedsInput = useRef(false);
  const travelling = useRef(false);
  const leaveTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const openTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const [engaged, setEngaged] = useState(false);
  const [phase, setPhase] = useState<Phase>('closed');
  const [placement, setPlacement] = useState<Placement>({ x: PREVIEW_GAP, y: PREVIEW_GAP, width: PREVIEW_WIDTH, maxHeight: PREVIEW_MAX_HEIGHT, side: 'right' });
  const requested = phase === 'open';
  const present = requested || paint.current.kind !== 'empty';
  const layer = useLayer({ mode: 'context', lazyMount: true });
  const { show, hide, ref: layerRef } = layer;
  const [layerHost, setLayerHost] = useState<HTMLElement | null>(null);
  const captureAnchor = useCallback((element: HTMLSpanElement | null) => {
    anchor.current = element;
    layerRef(element);
    // Viewport slots must sit outside ancestor paint/zoom. Keep them in the
    // nearest native dialog's top-layer scope, or the document's viewport root.
    setLayerHost(element === null ? null : element.closest('dialog') ?? element.ownerDocument.body);
  }, [layerRef, setLayerHost]);
  // Native hosting follows paint retention; interaction follows post-bind admission.
  useLayoutEffect(() => { if (present) show(); else hide(); }, [present, show, hide]);
  const admitted = () => {
    // Host reads may revoke synchronously. Inspect the publication afterwards.
    const ancestor = readParent(); const current = paint.current;
    return current.kind === 'admitted' && current.cycle === cycle.current
      && current.part === card.current && current.actual === actual.current
      && current.ancestor === ancestor && intent.current === 'open'
      && measurement.readBinding() === current.binding;
  };
  const parentView = (): ParentView => {
    const token = paint.current;
    return admitted() ? { kind: 'admitted', token, read: () => paint.current === token && admitted() }
      : { kind: intent.current === 'open' ? 'pending' : 'revoked' };
  };
  const readAdmission = useEffectEvent(admitted);
  const cancelLeave = useCallback(() => {
    travelling.current = false;
    if (leaveTimer.current !== null) clearTimeout(leaveTimer.current);
    leaveTimer.current = null;
  }, []);
  const cancelOpen = useCallback(() => {
    if (openTimer.current !== null) clearTimeout(openTimer.current);
    openTimer.current = null;
  }, []);
  const close = useCallback(() => {
    // Revoke the port before DOM permission, descendant intent and notifications.
    const old = paint.current;
    paint.current = old.kind === 'empty' ? old : { kind: 'retained', actual: old.actual, part: old.part };
    intent.current = 'closed'; cycle.current = {};
    gate(positioner.current, actual.current, card.current, false, old.kind !== 'empty');
    for (const child of nested.current) child.setParent({ kind: 'revoked' });
    if (old.kind === 'admitted') positioner.current?.dispatchEvent(new Event('nc-preview-layout', { bubbles: true }));
    cancelLeave(); cancelOpen();
    setEngaged(false);
    focusOnOpen.current = false;
    setPhase('closed');
  }, [cancelLeave, cancelOpen, setEngaged, setPhase]);
  const revoke = useCallback(() => {
    focusNeedsInput.current = true;
    const focused = document.activeElement;
    const ownedFocus = focused instanceof HTMLElement && card.current?.contains(focused);
    close();
    if (ownedFocus) focused.blur();
  }, [close]);
  const readHost = useInteractionScope(anchor, () => {
    revoke();
    const element = anchor.current;
    // Detachment revokes input, but keeps the current geometry carrier and
    // native paint identity for the already-owned exit/source freeze.
    if (element?.isConnected) setLayerHost(element.closest('dialog') ?? element.ownerDocument.body);
  });
  const readParent = useCallback(() => readHost() && parent.current.kind === 'admitted' && parent.current.read() ? parent.current.token : null, [readHost]);
  useEffect(() => {
    const tab = (event: KeyboardEvent) => {
      if (focusNeedsInput.current && event.key === 'Tab' && !event.defaultPrevented && !isImeKeyEvent(event) && readParent() !== null) focusNeedsInput.current = false;
    };
    document.addEventListener('keydown', tab);
    return () => { document.removeEventListener('keydown', tab); };
  }, [readParent]);
  const focusTrigger = useCallback(() => {
    if (!readHost()) return;
    skipFocus.current = true;
    const target = anchor.current?.querySelector<HTMLElement>('button, a, [tabindex]');
    if (target != null) restoreDrawerFocus(target);
    skipFocus.current = false;
  }, [readHost]);
  const dismissLayer = useCallback(() => {
    close();
    if (card.current?.contains(document.activeElement)) focusTrigger();
  }, [close, focusTrigger]);
  useLayerDismissal({
    isActive: requested, onDismiss: dismissLayer, getContainer: () => card.current,
    isPresent: () => admitted(),
  });
  const delayClose = useCallback((delay: number) => {
    const generation = cycle.current;
    leaveTimer.current = setTimeout(() => { if (cycle.current === generation) close(); }, delay);
  }, [close]);
  // The stable trigger endpoint exists before this preview can mount a portal.
  // Descendants register only with the explicit nearest owner, never a DOM broadcast.
  const registerChild = useEffectEvent((event: Event) => {
    event.stopPropagation();
    const child = (event as CustomEvent<NestedInteraction>).detail;
    nested.current.add(child);
    child.release = () => { nested.current.delete(child); };
    child.setParent(parentView());
  });
  useLayoutEffect(() => {
    const endpoint = anchor.current!;
    const register = (event: Event) => registerChild(event);
    endpoint.addEventListener(INTERACTION_REGISTER, register);
    return () => { endpoint.removeEventListener(INTERACTION_REGISTER, register); };
  }, []);
  const registeredParent = useRef<{ endpoint: HTMLElement; child: NestedInteraction } | null>(null);
  useLayoutEffect(() => {
    const trigger = anchor.current!;
    const owner = trigger.closest<HTMLElement>('[data-nc-preview-owner]');
    const endpoint = owner === null ? null : trigger.ownerDocument.getElementById(owner.dataset.ncPreviewOwner!);
    if (owner !== null && endpoint === null) throw new Error('Preview interaction owner is missing');
    if ((registeredParent.current?.endpoint ?? null) === endpoint) return;
    if (registeredParent.current !== null) {
      registeredParent.current.child.release?.(); registeredParent.current = null;
      revoke(); parent.current = { kind: 'admitted', token: {}, read: () => true };
    }
    if (endpoint === null) return;
    const child: NestedInteraction = { release: null, setParent: view => {
      if (parent.current.kind === view.kind && (view.kind !== 'admitted' || parent.current.kind === 'admitted' && parent.current.token === view.token)) return;
      parent.current = view;
      if (view.kind === 'revoked') revoke();
      schedule();
    } };
    endpoint.dispatchEvent(new CustomEvent(INTERACTION_REGISTER, { detail: child }));
    if (child.release === null) throw new Error('Preview interaction owner did not register child');
    registeredParent.current = { endpoint, child };
  });
  useLayoutEffect(() => () => {
    registeredParent.current?.child.release?.(); registeredParent.current = null;
  }, []);
  const dismissForNavigation = useCallback(() => {
    close();
    // Hand the destination a connected opener before it captures return focus.
    // Ancestors repeat this handoff, leaving focus on the root trigger.
    focusTrigger();
    // A nested portal's trigger lives inside its parent card. Propagate through
    // that trigger so navigation dismisses ancestors without closing unrelated previews.
    anchor.current?.dispatchEvent(new Event(NAVIGATION_DISMISS, { bubbles: true }));
  }, [close, focusTrigger]);
  useEffect(() => {
    const element = card.current;
    if (!requested || element === null) return;
    element.addEventListener(NAVIGATION_DISMISS, dismissForNavigation);
    return () => { element.removeEventListener(NAVIGATION_DISMISS, dismissForNavigation); };
  }, [requested, layer.isOpen, dismissForNavigation]);
  const activate = useCallback(() => {
    if (!readHost() || parent.current.kind === 'revoked') return;
    focusNeedsInput.current = false;
    cancelLeave(); cancelOpen(); intent.current = 'open'; setPhase('open'); schedule();
  }, [cancelLeave, cancelOpen, setPhase, schedule, readHost]);
  const focusContent = useCallback(() => {
    body.current?.focus({ preventScroll: true });
  }, []);
  const enter = () => {
    if (readParent() === null || focusNeedsInput.current) return;
    cancelLeave();
    if (intent.current === 'closed') intent.current = 'waiting';
    setEngaged(true);
    setPhase((current) => current === 'closed' ? 'waiting' : current);
  };
  const leave = () => {
    cancelLeave();
    setEngaged(false);
    delayClose(LEAVE_DELAY);
  };
  useEffect(() => () => {
    if (leaveTimer.current !== null) clearTimeout(leaveTimer.current);
  }, []);
  useEffect(() => {
    if (!engaged || phase !== 'waiting') return;
    const generation = cycle.current;
    const timeout = setTimeout(() => {
      if (readHost() && cycle.current === generation && intent.current !== 'open') activate();
    }, HOVER_DELAY);
    openTimer.current = timeout;
    return () => { clearTimeout(timeout); if (openTimer.current === timeout) openTimer.current = null; };
  }, [phase, engaged, activate, readHost]);

  // All input reads happen before submission. Output equality is exact; updaters
  // are pure. A committed slot is always usable, including while another is queued.
  const reposition = useEffectEvent(() => {
    if (!requested || readParent() === null) return false;
    const triggerElement = anchor.current;
    const cardElement = card.current;
    const slot = positioner.current;
    if (triggerElement === null || cardElement === null || slot === null) return;
    const source = triggerElement.getBoundingClientRect();
    const owner = triggerElement.closest<HTMLElement>('[data-nc-preview-owner]');
    const reading = owner ?? getReadingSurface?.(triggerElement) ?? null;
    const next = placePreview({
      anchor: source, reading: reading === null ? null : reading.getBoundingClientRect(),
      readingAreas: getAvoidSurfaces?.(triggerElement).map(element => element.getBoundingClientRect()),
      // Neige's own portal order is the total order. Retention keeps its position;
      // only earlier interactive reservations participate. There is no reverse edge.
      obstacles: Array.from(document.querySelectorAll<HTMLElement>('[data-nc-preview-slot]'))
        .filter(element => element !== slot && !element.inert
          && Boolean(element.compareDocumentPosition(slot) & Node.DOCUMENT_POSITION_FOLLOWING))
        .map(element => element.getBoundingClientRect()),
      viewport: { width: window.innerWidth, height: window.innerHeight },
    });
    if (next === null) {
      close();
      if (cardElement.contains(document.activeElement)) focusTrigger();
      cancelLeave(); return;
    }
    const equal = placement.x === next.x && placement.y === next.y && placement.width === next.width
      && placement.maxHeight === next.maxHeight && placement.side === next.side;
    if (!equal) setPlacement(next);
    return equal;
  });
  const measureContent = useEffectEvent(() => {
    if (!requested || readParent() === null) return false;
    if (header.current === null || intrinsic.current === null || card.current === null) return;
    // Computed border-box sizes retain layout fractions and exclude native paint.
    // Intrinsic flow is never constrained by the scrolling viewport.
    const size = (element: HTMLElement) => {
      const css = getComputedStyle(element);
      return parseFloat(css.height) + (css.boxSizing === 'border-box' ? 0
        : parseFloat(css.paddingTop) + parseFloat(css.paddingBottom) + parseFloat(css.borderTopWidth) + parseFloat(css.borderBottomWidth));
    };
    const css = getComputedStyle(card.current);
    const natural = size(header.current) + size(intrinsic.current)
      + parseFloat(css.borderTopWidth) + parseFloat(css.borderBottomWidth);
    const height = Math.min(natural, placement.maxHeight);
    if (!Number.isFinite(height) || height <= 0) return false;
    if (actualHeight !== height) setActualHeight(height);
    return actualHeight === height;
  });
  const updateLayout = useEffectEvent(() => {
    if (!requested || readParent() === null) return;
    const slotReady = reposition(); const frameReady = measureContent();
    // Prepare each newly mounted wrapper once, at its committed candidate width
    // and natural size. Subsequent committed frames remain valid during retarget.
    if (candidate !== actual.current && slotReady && frameReady && readSurfaceFrame(actual.current) !== null) setCandidate(actual.current);
  });
  useLayoutEffect(() => { updateLayout(); });
  // Rebind only for actual node/owner changes, never a placement/callback identity.
  const observed = useRef<{ nodes: readonly HTMLElement[]; dispose: () => void } | null>(null);
  useLayoutEffect(() => {
    const nodes = requested && anchor.current !== null && positioner.current !== null ? [anchor.current, header.current, intrinsic.current,
      actual.current, anchor.current.closest<HTMLElement>('[data-nc-preview-owner]'),
      getReadingSurface?.(anchor.current), ...(getAvoidSurfaces?.(anchor.current) ?? []),
      ...Array.from(document.querySelectorAll<HTMLElement>('[data-nc-preview-slot]'))
        .filter(element => element !== positioner.current && Boolean(element.compareDocumentPosition(positioner.current!) & Node.DOCUMENT_POSITION_FOLLOWING)),
    ].filter((node): node is HTMLElement => node != null) : [];
    if (observed.current !== null && observed.current.nodes.length === nodes.length
      && observed.current.nodes.every((node, index) => node === nodes[index])) return;
    observed.current?.dispose(); observed.current = null;
    if (nodes.length === 0) return;
    const observer = new ResizeObserver(() => { updateLayout(); });
    for (const node of nodes) observer.observe(node);
    observed.current = { nodes, dispose: () => observer.disconnect() };
  });
  useLayoutEffect(() => () => { observed.current?.dispose(); observed.current = null; }, []);
  useEffect(() => {
    if (!requested) return;
    const upstream = (event: Event) => {
      const earlier = event.target; const own = positioner.current;
      if (earlier instanceof HTMLElement && own !== null && earlier !== own
        && Boolean(earlier.compareDocumentPosition(own) & Node.DOCUMENT_POSITION_FOLLOWING)) schedule();
    };
    document.addEventListener('nc-preview-layout', upstream);
    return () => { document.removeEventListener('nc-preview-layout', upstream); };
  }, [requested, schedule]);
  useEffect(() => {
    if (!requested) return;
    const move = (event: PointerEvent) => {
      if (!readAdmission() || event.pointerType === 'touch' || leaveTimer.current === null || anchor.current === null || card.current === null) return;
      const target = event.target;
      const anotherControl = target instanceof Element && target.closest('button, a[href], input, select, textarea, [role="button"], [role="link"]') !== null
        && !anchor.current.contains(target) && !card.current.contains(target);
      if (!anotherControl && inPreviewBridge({ x: event.clientX, y: event.clientY },
        anchor.current.getBoundingClientRect(), card.current.getBoundingClientRect(), placement.side)) {
        cancelLeave();
        travelling.current = true;
        delayClose(TRAVEL_DELAY);
      } else if (travelling.current) {
        cancelLeave();
        delayClose(LEAVE_DELAY);
      }
    };
    document.addEventListener('pointermove', move);
    return () => { document.removeEventListener('pointermove', move); };
  }, [requested, placement.side, cancelLeave, delayClose]);
  useEffect(() => {
    if (!requested) return;
    const outsidePress = (event: PointerEvent) => {
      if (!readHost()) return;
      const target = event.target;
      if (!(target instanceof Node) || anchor.current?.contains(target)) return;
      // Descendant previews are portals; pressing within any preview remains interaction.
      if (target instanceof Element && target.closest('[data-nc-link-preview]') !== null) return;
      close();
    };
    const scroll = (event: Event) => {
      // Only scrolling an ancestor moves this preview's anchor; portal children own their scrolling.
      if (event.target instanceof Node && event.target.contains(anchor.current)
        && !card.current?.contains(event.target)) close();
    };
    window.addEventListener('resize', updateLayout);
    window.visualViewport?.addEventListener('resize', updateLayout);
    window.visualViewport?.addEventListener('scroll', updateLayout);
    document.addEventListener('pointerdown', outsidePress, true);
    document.addEventListener('scroll', scroll, true);
    return () => {
      window.removeEventListener('resize', updateLayout);
      window.visualViewport?.removeEventListener('resize', updateLayout);
      window.visualViewport?.removeEventListener('scroll', updateLayout);
      document.removeEventListener('pointerdown', outsidePress, true);
      document.removeEventListener('scroll', scroll, true);
    };
  }, [requested, focusTrigger, close, cancelLeave, readHost]);

  const measurement = useSurfacePresence({ parts: { kind: 'single', primary: { element: card, readFrame: () => candidate !== null && candidate === actual.current && (intent.current !== 'open' || readParent() !== null) ? readSurfaceFrame(actual.current) : null } },
    geometry: { kind: 'lift', origin: { kind: 'anchor', anchor: { ref: anchor }, unavailable: 'surface-center' } },
    open: requested, present, enabled: true, onMeasurementChange: schedule,
    // Native paint invalidates only later portals. The publisher never schedules
    // itself here; listeners schedule commits rather than publishing recursively.
    onVisualChange: () => { if (admitted()) positioner.current?.dispatchEvent(new Event('nc-preview-layout', { bubbles: true })); },
    onExited: () => { if (intent.current === 'open') return; paint.current = { kind: 'empty' }; setCandidate(null); schedule(); } });

  // This phase is deliberately after motion's committed binding update. Notices
  // only schedule this local commit; publication never runs in a notice stack.
  useLayoutEffect(() => {
    const binding = measurement.readBinding();
    const before = paint.current;
    const part = card.current; const node = actual.current;
    const ancestor = readParent();
    const valid = intent.current === 'open' && ancestor !== null && node !== null && part !== null
      && candidate === node && binding.kind === 'ready' && binding.parts.length === 1 && binding.parts[0].element === part;
    if (valid) paint.current = { kind: 'admitted', actual: node, part, binding, cycle: cycle.current, ancestor, slot: placement, height: actualHeight };
    else if (before.kind !== 'empty') paint.current = { kind: intent.current === 'open' ? 'suspended' : 'retained', actual: before.actual, part: before.part };
    let current = paint.current;
    const changed = current.kind === 'admitted' ? before.kind !== 'admitted' || before.binding !== current.binding
      || before.cycle !== current.cycle || before.ancestor !== current.ancestor || before.actual !== current.actual || before.part !== current.part
      || before.slot !== current.slot || before.height !== current.height : before.kind === 'admitted';
    if (current.kind === 'admitted' && !changed) { paint.current = before; current = before; }
    gate(positioner.current, node, part, valid, valid || current.kind === 'retained' && current.actual === node && current.part === part);
    if (valid && focusOnOpen.current) { focusOnOpen.current = false; focusContent(); }
    if (paint.current !== current || valid && !admitted()) return;
    if (changed) {
      inform(parentView());
      positioner.current?.dispatchEvent(new Event('nc-preview-layout', { bubbles: true }));
    }
  });
  useLayoutEffect(() => () => {
    cycle.current = {};
    paint.current = { kind: 'empty' };
    gate(positioner.current, actual.current, card.current, false, false);
  }, []);
  const attachSlot = useCallback((node: HTMLDivElement | null) => { positioner.current = node; if (node !== null) gate(node, null, null, false, false); }, []);
  const attachActual = useCallback((node: HTMLDivElement | null) => { actual.current = node; if (node !== null) gate(null, node, null, false, false); }, []);
  const attachCard = useCallback((node: HTMLDivElement | null) => { card.current = node; if (node !== null) gate(null, null, node, false, false); }, []);

  return <span ref={captureAnchor} id={`${id}-owner`} className={styles.anchor} role="presentation"
    onPointerEnter={(event) => {
      if (event.pointerType !== 'touch' && event.target instanceof Node && anchor.current?.contains(event.target) && readHost() && !focusNeedsInput.current) {
        focusNeedsInput.current = false; enter();
      }
    }}
    onPointerMove={event => {
      if (focusNeedsInput.current && event.pointerType !== 'touch' && (event.movementX !== 0 || event.movementY !== 0)
        && event.target instanceof Node && anchor.current?.contains(event.target) && readHost()) {
        focusNeedsInput.current = false; enter();
      }
    }}
    onPointerLeave={event => { if (intent.current !== 'closed' && readParent() !== null && event.target instanceof Node && anchor.current?.contains(event.target)) leave(); }} onFocus={() => { if (!skipFocus.current && !focusNeedsInput.current) enter(); }}
    onBlur={(event) => {
      if (intent.current === 'closed') return;
      if (readParent() === null || !admitted() && event.target instanceof Node && card.current?.contains(event.target)) return;
      if (event.relatedTarget instanceof Node && (anchor.current?.contains(event.relatedTarget)
        || card.current?.contains(event.relatedTarget))) return;
      leave();
    }}
    onKeyDown={(event) => {
      if (event.defaultPrevented || isImeKeyEvent(event.nativeEvent)) return;
      // React portals bubble through this span; only its actual trigger owns activation.
      if (!(event.target instanceof Node) || !anchor.current?.contains(event.target)) return;
      if (!readHost() || parent.current.kind === 'revoked') return;
      if (event.key === 'ArrowDown') {
        event.preventDefault();
        activate();
        if (!admitted()) focusOnOpen.current = true; else focusContent();
      }
      if (event.key === 'Escape' && !admitted() && phase !== 'closed') { event.preventDefault(); close(); }
    }}>
    {trigger(activate, dismissForNavigation)}
    {layerHost !== null && createPortal(layer.render(<LayerDepthProvider>{present && <div ref={attachSlot} className={styles.positioner} data-nc-preview-slot="" inert aria-hidden="true"
      onClickCapture={event => { if (!admitted()) { event.preventDefault(); event.stopPropagation(); } }}
      onPointerDownCapture={event => { if (!admitted()) { event.preventDefault(); event.stopPropagation(); } }}
      onFocusCapture={event => { if (!admitted()) { event.stopPropagation(); (event.target as HTMLElement).blur(); } }}
      style={{ left: placement.x, top: placement.y, width: placement.width, height: placement.maxHeight, justifyContent: placement.side === 'above' ? 'flex-end' : 'flex-start' }}>
      <div ref={attachActual} className={styles.actual} style={{ height: Math.min(actualHeight, placement.maxHeight), clipPath: 'inset(50%)' }}
      onPointerEnter={() => { if (intent.current === 'open') enter(); }} onPointerLeave={event => {
        // The portal is a logical child of the trigger span. Returning to that
        // ancestor does not deliver another trigger enter to cancel a leave.
        if (admitted() && !(event.relatedTarget instanceof Node && anchor.current?.contains(event.relatedTarget))) leave();
      }}>
      <CardFrame ref={attachCard} className={styles.card} role="dialog" data-nc-preview-owner={`${id}-owner`}
      aria-label={`Preview: ${title}`} id={id} inert aria-hidden="true"
      onFocus={() => { if (intent.current === 'open') enter(); }} onBlur={(event) => {
        if (!admitted()) return;
        if (event.relatedTarget instanceof Node && card.current?.contains(event.relatedTarget)) return;
        leave();
      }}>
      <div ref={header} className={styles.header}>
        <span className={styles.title}>{title}</span>
      </div>
      <div ref={body} className={styles.body} tabIndex={-1} role="region" aria-label={`Preview content: ${title}`}>
        <div ref={intrinsic} className={styles.intrinsic}>{typeof children === 'function' ? children(dismissForNavigation) : children}</div>
      </div>
    </CardFrame></div></div>}</LayerDepthProvider>, { positioning: 'custom',
      // Layer owns native presentation and safe hosting; the slot/actual/card
      // retain viewport placement, measurement and physical paint ownership.
      style: { position: 'fixed', inset: 0, width: 0, height: 0, margin: 0, padding: 0,
        border: 0, overflow: 'visible', background: 'transparent' },
    }), layerHost)}
  </span>;
}
