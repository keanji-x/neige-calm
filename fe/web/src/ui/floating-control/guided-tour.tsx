import { createContext, useCallback, useContext, useEffect, useRef, type ReactNode, type RefCallback } from 'react';
import { createPortal } from 'react-dom';
import { Button } from '@astryxdesign/core/Button';
import { useState } from '../state/public.ts';
import { AnchoredHint, supportsAnchoredHints } from './anchored-hint.tsx';
import type { GuideCatalog, GuideStep } from './guide-catalog.ts';
import styles from './anchored-hint.module.css';
import material from './floating-control.module.css';

export const guideRevealSlot = Object.freeze({ 'data-nc-guide-reveal': '' });

function targetIds(step: GuideStep): readonly string[] { return typeof step.target === 'string' ? [step.target] : step.target; }
type Target = Readonly<{ id: string; element: HTMLElement; ready: boolean }>;
type Current = Readonly<{ step: GuideStep; element: HTMLElement | null; locating: boolean; index: number; count: number; tour: string }>;
type GuideContextValue = Readonly<{
  register: (key: symbol, target: Target | null) => void;
  current: Current | null;
  next: () => void; previous: (() => void) | undefined; skip: () => void;
}>;
const GuideContext = createContext<GuideContextValue | null>(null);

function visible(element: HTMLElement): boolean {
  if (!element.isConnected || element.closest('[hidden], [inert], [aria-hidden="true"]')) return false;
  const box = element.getBoundingClientRect();
  let opacity = 1;
  for (let node: HTMLElement | null = element; node !== null; node = node.parentElement) {
    if (!node.hasAttribute('data-nc-guide-reveal')) opacity *= Number(getComputedStyle(node).opacity);
    if (opacity <= 0.01) return false;
  }
  return box.width > 0 && box.height > 0 && getComputedStyle(element).visibility !== 'hidden';
}

/** One callout at a time; targets are declared by their owners, never discovered by application selectors. */
export function GuideProvider({ catalog, enabled, sessionKey, facts, isSeen, onSeen, onSkip, children }: Readonly<{
  catalog: GuideCatalog; enabled: boolean; sessionKey: string; facts: Readonly<Record<string, boolean>>; isSeen: (step: string) => boolean;
  onSeen: (step: string) => void; onSkip: () => void; children: ReactNode;
}>) {
  const running = enabled && supportsAnchoredHints();
  const [targets] = useState(() => new Map<symbol, Target>());
  const [, revise] = useState(0);
  const [pinned, setPinned] = useState<string | null>(null);
  useEffect(() => setPinned(null), [sessionKey]);
  const register = useCallback((key: symbol, target: Target | null) => {
    const previous = targets.get(key);
    if (previous?.id === target?.id && previous?.element === target?.element && previous?.ready === target?.ready) return;
    if (target === null) targets.delete(key); else targets.set(key, target);
    revise(value => value + 1);
  }, [targets]);
  useEffect(() => {
    if (!running) return;
    let frame = 0;
    let last: readonly unknown[] = [];
    const refresh = () => {
      cancelAnimationFrame(frame);
      frame = requestAnimationFrame(() => {
        const modal = [...document.querySelectorAll<HTMLElement>('[role="dialog"][aria-modal="true"]')].filter(visible).at(-1);
        const menu = [...document.querySelectorAll<HTMLElement>('[role="menu"], [role="listbox"], [popover]:popover-open:not([data-nc-guide-hint]):not([data-nc-guide-ring])')].some(visible);
        const snapshot = [modal, menu, ...[...targets].flatMap(([key, target]) => [key, visible(target.element)])];
        if (snapshot.length === last.length && snapshot.every((value, index) => value === last[index])) return;
        last = snapshot;
        revise(value => value + 1);
      });
    };
    const observer = new MutationObserver(refresh);
    observer.observe(document.body, { subtree: true, childList: true, attributes: true,
      attributeFilter: ['hidden', 'inert', 'aria-hidden', 'open', 'style', 'class', 'data-nc-guide-reveal'] });
    window.addEventListener('resize', refresh);
    document.addEventListener('toggle', refresh, true);
    refresh();
    return () => { observer.disconnect(); cancelAnimationFrame(frame); window.removeEventListener('resize', refresh); document.removeEventListener('toggle', refresh, true); };
  }, [running, targets]);
  const available = (() => {
    if (!running) return [];
    // Native menus and nested pickers temporarily own attention and focus.
    if ([...document.querySelectorAll<HTMLElement>('[role="menu"], [role="listbox"], [popover]:popover-open:not([data-nc-guide-hint]):not([data-nc-guide-ring])')].some(visible)) return [];
    const modal = [...document.querySelectorAll<HTMLElement>('[role="dialog"][aria-modal="true"]')].filter(visible).at(-1);
    const registered = [...targets.values()];
    const usable = registered.filter(target => target.ready && visible(target.element) && (modal === undefined || modal.contains(target.element)));
    return catalog.tours.filter(tour => tour.requires.every(fact => facts[fact] === true)).map(tour => ({ tour, steps: tour.steps.flatMap(step => {
      const ids = targetIds(step);
      const target = usable.find(candidate => ids.includes(candidate.id));
      const via = step.via?.map(id => usable.find(candidate => candidate.id === id)).find(candidate => candidate !== undefined);
      const mounted = registered.some(candidate => ids.includes(candidate.id) && (modal === undefined || modal.contains(candidate.element)));
      if (!mounted && step.locate === undefined && step.via === undefined) return [];
      return [{ step, element: target?.element ?? via?.element ?? null, locating: target === undefined }];
    }) })).filter(entry => entry.steps.length > 0).sort((a, b) => b.tour.priority - a.tour.priority);
  })();
  // Completed exclusive scenes no longer prevent unseen help elsewhere.
  const scene = available.find(entry => entry.steps.some(({ step }) => step.id === pinned || !isSeen(step.id)))
    ?? (running ? [...catalog.tours].filter(tour => tour.requires.every(fact => facts[fact] === true)
      && tour.steps.some(step => !isSeen(step.id))).sort((a, b) => b.priority - a.priority)
      .map(tour => ({ tour, steps: tour.steps.map(step => ({ step, element: null, locating: true })) }))[0] : undefined);
  const candidates = scene?.steps ?? [];
  const selected = candidates.find(entry => entry.step.id === pinned) ?? candidates.find(entry => !isSeen(entry.step.id));
  const index = selected === undefined ? -1 : candidates.indexOf(selected);
  const current = selected === undefined || scene === undefined ? null
    : { ...selected, index, count: candidates.length, tour: scene.tour.title };
  const next = () => {
    if (current === null) return;
    onSeen(current.step.id);
    setPinned(pinned !== null ? candidates[index + 1]?.step.id ?? null : null);
  };
  const previous = index > 0 ? () => setPinned(candidates[index - 1].step.id) : undefined;
  const currentElement = current?.element;
  const currentId = current?.step.id;
  useEffect(() => {
    if (currentElement == null) return;
    const bounds = currentElement.getBoundingClientRect();
    let clipped = bounds.top < 0 || bounds.bottom > window.innerHeight || bounds.left < 0 || bounds.right > window.innerWidth;
    for (let parent = currentElement.parentElement; parent !== null; parent = parent.parentElement) {
      const style = getComputedStyle(parent);
      const rect = parent.getBoundingClientRect();
      if (/(auto|scroll|hidden|clip)/.test(style.overflowX) && (bounds.left < rect.left || bounds.right > rect.right)) clipped = true;
      if (/(auto|scroll|hidden|clip)/.test(style.overflowY) && (bounds.top < rect.top || bounds.bottom > rect.bottom)) clipped = true;
    }
    if (clipped) currentElement.scrollIntoView({ block: 'nearest', inline: 'nearest' });
  }, [currentElement, currentId]);
  const statusHost = running ? [...document.querySelectorAll<HTMLElement>('[role="dialog"][aria-modal="true"]')].filter(visible).at(-1) ?? document.body : null;
  const value = { register, current, next, previous, skip: onSkip };
  return <GuideContext.Provider value={value}>{children}
    {current?.element === null && statusHost !== null && createPortal(<div role="status" aria-label="引导下一步" className={`${styles.status} ${styles.hint} ${material.material}`}>
      <span className={styles.context}>{current.tour} · 下一步：{current.step.title}</span>
      <p>{current.step.locate ?? current.step.description}</p>
      <Button label="跳过引导" size="sm" variant="ghost" onClick={onSkip} />
    </div>, statusHost)}
  </GuideContext.Provider>;
}

export function useGuideTargetActive(id: string): boolean {
  const current = useContext(GuideContext)?.current;
  return current != null && !current.locating && targetIds(current.step).includes(id);
}

function BoundGuideControl({ anchor, onElement, describedBy, children }: Readonly<{
  anchor: RefCallback<HTMLElement>; onElement: (node: HTMLElement | null) => void;
  describedBy: string | undefined;
  children: (anchor: RefCallback<HTMLElement>, describedBy: string | undefined) => ReactNode;
}>) {
  const bind = useCallback((node: HTMLElement | null) => { anchor(node); onElement(node); }, [anchor, onElement]);
  return children(bind, describedBy);
}

/** The hint remains inside the target's own Dialog/form ancestry. Group nearby controls with one target. */
export function GuideAnchor({ id, disabled = false, reserveSpace = false, placement = 'bottom', onExample, canAdvance = true, children }: Readonly<{
  id: string; disabled?: boolean; reserveSpace?: boolean; placement?: 'top' | 'bottom';
  /** The target owner may insert a repository-authored example into its empty draft. Never submits. */
  onExample?: (text: string) => void;
  /** Local readiness of this explanation's native control; never changes the product's own validation. */
  canAdvance?: boolean;
  children: (anchor: RefCallback<HTMLElement>, describedBy: string | undefined) => ReactNode;
}>) {
  const guide = useContext(GuideContext);
  const key = useRef(Symbol());
  const [element, setElement] = useState<HTMLElement | null>(null);
  const register = guide?.register;
  useEffect(() => {
    register?.(key.current, element !== null ? { id, element, ready: !disabled } : null);
    const token = key.current;
    return () => register?.(token, null);
  }, [register, id, element, disabled]);
  const active = guide !== null && guide.current !== null && guide.current.element === element && (guide.current.locating ? (guide.current.step.via?.includes(id) ?? false) : targetIds(guide.current.step).includes(id)) && !disabled;
  const advance = guide?.current?.step.advance;
  const next = guide?.next;
  const step = active && !guide.current.locating ? guide.current.step : null;
  const example = step?.example;
  const latestElement = useRef(element);
  latestElement.current = element;
  const latestId = useRef(id);
  latestId.current = id;
  const latestDisabled = useRef(disabled);
  latestDisabled.current = disabled;
  const armed = useRef<(() => void) | null>(null);
  useEffect(() => {
    if (!active || element === null) return;
    element.setAttribute('data-nc-guide-target', '');
    return () => { element.removeAttribute('data-nc-guide-target'); };
  }, [active, element]);
  useEffect(() => {
    if (!active || guide?.current?.locating || !canAdvance || element === null || advance !== 'activate' || next === undefined) return;
    let finished = false;
    const finish = () => { if (!finished) { finished = true; armed.current = null; next(); } };
    let armExpiry: ReturnType<typeof setTimeout> | undefined;
    const arm = () => {
      armed.current = finish;
      clearTimeout(armExpiry);
      armExpiry = setTimeout(() => { if (armed.current === finish) armed.current = null; }, 0);
    };
    const keydown = (event: KeyboardEvent) => { if (event.key === 'Enter' || event.key === ' ') arm(); };
    const cancel = () => { armed.current = null; };
    element.addEventListener('pointerdown', arm, true);
    element.addEventListener('keydown', keydown, true);
    element.addEventListener('pointercancel', cancel);
    element.addEventListener('click', finish);
    return () => {
      clearTimeout(armExpiry);
      element.removeEventListener('pointerdown', arm, true);
      element.removeEventListener('keydown', keydown, true);
      element.removeEventListener('pointercancel', cancel);
      element.removeEventListener('click', finish);
      // Withdrawal during the native activation must not lose the advance.
      if (armed.current === finish) {
        armed.current = null;
        if (!element.isConnected || element !== latestElement.current || latestDisabled.current || id !== latestId.current) finish();

      }
    };
  }, [active, canAdvance, element, advance, next, guide?.current?.locating]);
  const act = (action: (() => void) | undefined) => {
    element?.focus({ preventScroll: true });
    action?.();
  };
  return <AnchoredHint open={active} title={active ? guide.current.step.title : ''}
    description={active ? guide.current.locating ? guide.current.step.locate ?? guide.current.step.description : guide.current.step.description : ''} reserveSpace={reserveSpace} placement={placement}
    onSkip={() => guide?.skip()}
    contextLabel={active ? `${guide.current.tour} · ${guide.current.index + 1} / ${guide.current.count}` : undefined}
    detail={step === null ? undefined : <>
      {example !== undefined && <div className={styles.example}>
        <span className={styles.exampleLabel}>试一试</span>
        <p>{example.summary}</p>
        {onExample !== undefined && <Button type="button" label={example.label} size="sm" variant="secondary"
          onClick={() => { element?.focus({ preventScroll: true }); onExample(example.text); }} />}
      </div>}
      {step.outcome !== undefined && <p className={styles.outcome}>接下来：{step.outcome}</p>}
    </>}
    footer={active ? <>
      <div className={styles.actions}>
        <Button type="button" className={styles.skip} label="跳过引导" size="sm" variant="ghost" onClick={() => act(guide.skip)} />
        {guide.previous !== undefined && <Button label="上一步" size="sm" variant="ghost" onClick={() => act(guide.previous)} />}
        {!guide.current.locating && <Button type="button" label={advance === 'activate' ? '略过这一步' : guide.current.index + 1 === guide.current.count ? '知道了' : '下一步'} isDisabled={!canAdvance} size="sm" variant={advance === 'activate' ? 'ghost' : 'primary'} onClick={() => act(guide.next)} />}
      </div>
    </> : undefined}>
    {(anchor, describedBy) => <BoundGuideControl anchor={anchor} onElement={setElement} describedBy={describedBy}>{children}</BoundGuideControl>}
  </AnchoredHint>;
}
