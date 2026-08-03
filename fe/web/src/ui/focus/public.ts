import type { KeyboardEvent as ReactKeyboardEvent } from 'react';
import { useCallback, useEffect, useRef } from 'react';
import { useState } from '../state/public.ts';

export interface RovingOptions {
  itemCount: number;
  initialIndex?: number;
  loop?: boolean;
  onActivate?: (index: number) => void;
  onEscape?: () => void;
  getLabel?: (index: number) => string;
  typeaheadTimeoutMs?: number;
}

export interface RovingItemProps<T extends HTMLElement> {
  ref: (element: T | null) => void;
  tabIndex: number;
  onKeyDown: (event: ReactKeyboardEvent) => void;
}

export interface RovingResult<T extends HTMLElement> {
  activeIndex: number;
  setActiveIndex: (index: number) => void;
  getItemProps: (index: number) => RovingItemProps<T>;
}

export function normalizeTypeaheadLabel(label: string): string {
  return label.trim().toLowerCase();
}

export function findTypeaheadMatch(labels: readonly string[], buffer: string, startFrom: number): number {
  const needle = normalizeTypeaheadLabel(buffer);
  if (!needle || labels.length === 0) return -1;
  const start = needle.length > 1 ? startFrom : startFrom + 1;
  for (let offset = 0; offset < labels.length; offset += 1) {
    const index = ((start + offset) % labels.length + labels.length) % labels.length;
    if (normalizeTypeaheadLabel(labels[index] ?? '').startsWith(needle)) return index;
  }
  return -1;
}

export function useRovingTabindex<T extends HTMLElement>({
  itemCount, initialIndex = 0, loop = true, onActivate, onEscape, getLabel,
  typeaheadTimeoutMs = 500,
}: RovingOptions): RovingResult<T> {
  const clamp = (index: number) => Math.min(Math.max(index, 0), Math.max(itemCount - 1, 0));
  const [activeIndex, setIndex] = useState(clamp(initialIndex));
  const refs = useRef<Array<T | null>>([]);
  const buffer = useRef('');
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);

  useEffect(() => {
    if (itemCount > 0 && activeIndex >= itemCount) setIndex(itemCount - 1);
  }, [activeIndex, itemCount, setIndex]);
  useEffect(() => { refs.current[activeIndex]?.focus(); }, [activeIndex]);
  useEffect(() => () => { if (timer.current !== null) clearTimeout(timer.current); }, []);

  const setActiveIndex = useCallback((index: number) => {
    setIndex(index);
    refs.current[index]?.focus();
  }, [setIndex]);

  const move = useCallback((delta: number) => {
    if (itemCount === 0) return;
    const raw = activeIndex + delta;
    setIndex(loop ? ((raw % itemCount) + itemCount) % itemCount : Math.min(Math.max(raw, 0), Math.max(itemCount - 1, 0)));
  }, [activeIndex, itemCount, loop, setIndex]);

  const appendTypeahead = useCallback((character: string) => {
    if (!getLabel || itemCount === 0) return;
    buffer.current += character;
    const labels = Array.from({ length: itemCount }, (_, index) => getLabel(index));
    const match = findTypeaheadMatch(labels, buffer.current, activeIndex);
    if (match >= 0) setIndex(match);
    if (timer.current !== null) clearTimeout(timer.current);
    timer.current = setTimeout(() => { buffer.current = ''; timer.current = null; }, typeaheadTimeoutMs);
  }, [activeIndex, getLabel, itemCount, setIndex, typeaheadTimeoutMs]);

  const onKeyDown = useCallback((event: ReactKeyboardEvent) => {
    switch (event.key) {
      case 'ArrowDown': event.preventDefault(); move(1); return;
      case 'ArrowUp': event.preventDefault(); move(-1); return;
      case 'Home': event.preventDefault(); if (itemCount > 0) setIndex(0); return;
      case 'End': event.preventDefault(); if (itemCount > 0) setIndex(itemCount - 1); return;
      case 'Enter': event.preventDefault(); onActivate?.(activeIndex); return;
      case 'Escape': event.preventDefault(); onEscape?.(); return;
      case ' ':
        event.preventDefault();
        if (buffer.current && getLabel) appendTypeahead(' '); else onActivate?.(activeIndex);
        return;
      default:
        if (getLabel && event.key.length === 1 && !event.ctrlKey && !event.metaKey && !event.altKey) {
          event.preventDefault(); appendTypeahead(event.key);
        }
    }
    // ArrowLeft and ArrowRight intentionally pass through unchanged for vertical composites.
  }, [activeIndex, appendTypeahead, getLabel, itemCount, move, onActivate, onEscape, setIndex]);

  const getItemProps = useCallback((index: number): RovingItemProps<T> => ({
    ref: (element) => {
      refs.current[index] = element;
      if (element && index === activeIndex) queueMicrotask(() => {
        if (refs.current[index] === element && document.contains(element)) element.focus();
      });
    },
    tabIndex: index === activeIndex ? 0 : -1,
    onKeyDown,
  }), [activeIndex, onKeyDown]);

  return { activeIndex, setActiveIndex, getItemProps };
}
