import { createContext, useCallback, useContext, useEffect, useRef, type ReactNode, type RefCallback } from 'react';
import { Button } from '@astryxdesign/core/Button';
import { useState } from '../state/public.ts';
import { AnchoredHint, supportsAnchoredHints } from './anchored-hint.tsx';
import type { GuideCatalog, GuideStep } from './guide-catalog.ts';
import styles from './anchored-hint.module.css';

type Target = Readonly<{ id: string; element: HTMLElement }>;
type Current = Readonly<{ step: GuideStep; element: HTMLElement; index: number; count: number; tour: string }>;
type GuideContextValue = Readonly<{
  register: (key: symbol, target: Target | null) => void;
  current: Current | null;
  next: () => void; previous: (() => void) | undefined; skip: () => void;
}>;
const GuideContext = createContext<GuideContextValue | null>(null);

function visible(element: HTMLElement): boolean {
  if (!element.isConnected || element.closest('[hidden], [inert], [aria-hidden="true"]')) return false;
  const box = element.getBoundingClientRect();
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
    if (previous?.id === target?.id && previous?.element === target?.element) return;
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
        const menu = [...document.querySelectorAll<HTMLElement>('[role="menu"], [role="listbox"], [popover]:popover-open:not([data-nc-guide-hint])')].some(visible);
        const snapshot = [modal, menu, ...[...targets].flatMap(([key, target]) => [key, visible(target.element)])];
        if (snapshot.length === last.length && snapshot.every((value, index) => value === last[index])) return;
        last = snapshot;
        revise(value => value + 1);
      });
    };
    const observer = new MutationObserver(refresh);
    observer.observe(document.body, { subtree: true, childList: true, attributes: true,
      attributeFilter: ['hidden', 'inert', 'aria-hidden', 'open', 'style', 'class'] });
    window.addEventListener('resize', refresh);
    document.addEventListener('toggle', refresh, true);
    refresh();
    return () => { observer.disconnect(); cancelAnimationFrame(frame); window.removeEventListener('resize', refresh); document.removeEventListener('toggle', refresh, true); };
  }, [running, targets]);
  const available = (() => {
    if (!running) return [];
    // Native menus and nested pickers temporarily own attention and focus.
    if ([...document.querySelectorAll<HTMLElement>('[role="menu"], [role="listbox"], [popover]:popover-open:not([data-nc-guide-hint])')].some(visible)) return [];
    const modal = [...document.querySelectorAll<HTMLElement>('[role="dialog"][aria-modal="true"]')].filter(visible).at(-1);
    const usable = [...targets.values()].filter(target => visible(target.element) && (modal === undefined || modal.contains(target.element)));
    return catalog.tours.filter(tour => tour.requires.every(fact => facts[fact] === true)).map(tour => ({ tour, steps: tour.steps.flatMap(step => {
      const target = usable.find(candidate => candidate.id === step.target);
      return target === undefined ? [] : [{ step, element: target.element }];
    }) })).filter(entry => entry.steps.length > 0).sort((a, b) => b.tour.priority - a.tour.priority);
    // DOM visibility is deliberately invalidated by target registration and modal changes.
  })();
  // An exclusive foreground scene may pause completed help until it closes.
  const boundary = available.findIndex(entry => entry.tour.exclusive);
  const scenes = boundary < 0 ? available : available.slice(0, boundary + 1);
  const scene = scenes.find(entry => entry.steps.some(({ step }) => step.id === pinned || !isSeen(step.id)));
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
    if (currentElement === undefined) return;
    const bounds = currentElement.getBoundingClientRect();
    if (bounds.top < 0 || bounds.bottom > window.innerHeight) currentElement.scrollIntoView({ block: 'nearest' });
  }, [currentElement, currentId]);
  const value = { register, current, next, previous, skip: onSkip };
  return <GuideContext.Provider value={value}>{children}</GuideContext.Provider>;
}

export function useGuideTargetActive(id: string): boolean {
  return useContext(GuideContext)?.current?.step.target === id;
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
    register?.(key.current, !disabled && element !== null ? { id, element } : null);
    const token = key.current;
    return () => register?.(token, null);
  }, [register, id, element, disabled]);
  const active = guide !== null && guide.current !== null && guide.current.element === element && guide.current.step.target === id && !disabled;
  const advance = guide?.current?.step.advance;
  const next = guide?.next;
  const step = active ? guide.current.step : null;
  const example = step?.example;
  useEffect(() => {
    if (!active || !canAdvance || element === null || advance !== 'activate' || next === undefined) return;
    element.addEventListener('click', next);
    return () => element.removeEventListener('click', next);
  }, [active, canAdvance, element, advance, next]);
  const act = (action: (() => void) | undefined) => {
    element?.focus({ preventScroll: true });
    action?.();
  };
  return <AnchoredHint open={active} title={active ? guide.current.step.title : ''}
    description={active ? guide.current.step.description : ''} reserveSpace={reserveSpace} placement={placement}
    onSkip={() => guide?.skip()}
    detail={step === null ? undefined : <>
      {example !== undefined && <div className={styles.example}>
        <span className={styles.title}>可以先试这个</span>
        <p>{example.summary}</p>
        {onExample !== undefined && <Button type="button" label={example.label} size="sm" variant="secondary"
          onClick={() => { element?.focus({ preventScroll: true }); onExample(example.text); }} />}
      </div>}
      {step.outcome !== undefined && <p className={styles.outcome}>接下来：{step.outcome}</p>}
    </>}
    footer={active ? <>
      <span className={styles.progress}>{guide.current.tour} · {guide.current.index + 1}/{guide.current.count}</span>
      <div className={styles.actions}>
        <Button label="跳过引导" size="sm" variant="ghost" onClick={() => act(guide.skip)} />
        {guide.previous !== undefined && <Button label="上一步" size="sm" variant="ghost" onClick={() => act(guide.previous)} />}
        <Button type="button" label={guide.current.index + 1 === guide.current.count ? '知道了' : '下一步'} isDisabled={!canAdvance} size="sm" variant="secondary" onClick={() => act(guide.next)} />
      </div>
    </> : undefined}>
    {(anchor, describedBy) => <BoundGuideControl anchor={anchor} onElement={setElement} describedBy={describedBy}>{children}</BoundGuideControl>}
  </AnchoredHint>;
}
