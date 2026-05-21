// useRovingTabindex — keyboard navigation for menu / listbox composites.
//
// What it implements
// ------------------
// The **roving tabindex** pattern from WAI-ARIA APG: exactly one item in
// the composite is in the page Tab sequence (`tabIndex={0}`); all others
// are `tabIndex={-1}`. Arrow keys move the active index *within* the
// composite — the user sees focus jump from item to item — without
// growing the Tab order. This is the canonical pattern for menus,
// listboxes, toolbars, and grids; here we use it for the AddPanel menu
// (Slice 7 of issue #56).
//
// Public API
// ----------
//   const { activeIndex, setActiveIndex, getItemProps } =
//     useRovingTabindex({ itemCount, onActivate, onEscape });
//
//   {items.map((item, i) => (
//     <button key={item.id} {...getItemProps(i)}>
//       {item.label}
//     </button>
//   ))}
//
// Behavior contract
// -----------------
//   - **ArrowDown** — next item; wraps to first if `loop` (default true).
//   - **ArrowUp**   — previous item; wraps to last if `loop`.
//   - **Home**      — first item.
//   - **End**       — last item.
//   - **Enter / Space** — call `onActivate(activeIndex)`; `preventDefault`.
//   - **Escape**    — call `onEscape?.()`; `preventDefault`.
//   - **ArrowLeft / ArrowRight** — intentionally NOT handled. A vertical
//     menu must not consume horizontal arrows: caret navigation inside any
//     editable descendant relies on them. (Orientation is hard-coded
//     vertical for now — YAGNI; future composite-orientation needs can
//     extend this with an `orientation` opt.)
//   - **Type-ahead** — when `getLabel` is supplied, printable characters
//     buffer for ~500ms and the active index jumps to the first item
//     whose label *starts with* the buffer (case-insensitive). Matches
//     the WAI-ARIA APG menu typeahead behavior.
//
// Focus side-effect: the hook focuses the item whose ref is registered at
// the active index whenever `activeIndex` changes *and* the matching ref
// is mounted. Callers that need to suppress this (e.g. closing a popover
// before unmount) can simply unmount — the hook noops on missing refs.
//
// Hand-rolled rather than pulling a headless-UI library; see issue #56
// architecture note + `docs/a11y-contract.md` §10.

import { useCallback, useEffect, useRef } from 'react';
import { useState } from '../shared/state';
import type { KeyboardEvent as ReactKeyboardEvent } from 'react';

export interface UseRovingTabindexOptions {
  /** Total number of items in the composite. The hook clamps
   *  `activeIndex` into `[0, itemCount-1]` when this changes. */
  itemCount: number;
  /** Index focused on first mount. Defaults to 0. */
  initialIndex?: number;
  /** Wrap at boundaries (true) or clamp (false). Default true. */
  loop?: boolean;
  /** Fired on Enter / Space with the current active index. */
  onActivate?: (index: number) => void;
  /** Fired on Escape. */
  onEscape?: () => void;
  /**
   * Optional label getter for type-ahead. When supplied, printable keys
   * buffer for ~500ms and the active index jumps to the first item whose
   * label starts with the buffer (case-insensitive).
   *
   * Return the *visible* label — the same string a user sees. Trimmed
   * internally before comparison.
   */
  getLabel?: (index: number) => string;
  /**
   * Typeahead idle reset window in ms. Default 500ms — matches the WAI-
   * ARIA APG reference impl. Tests can shrink this for determinism.
   */
  typeaheadTimeoutMs?: number;
}

export interface RovingItemProps<T extends HTMLElement> {
  ref: (el: T | null) => void;
  tabIndex: number;
  onKeyDown: (e: ReactKeyboardEvent) => void;
}

export interface UseRovingTabindexResult<T extends HTMLElement> {
  activeIndex: number;
  /** Imperative setter — exposed so callers can force-focus a specific
   *  item on open / programmatic events. Also focuses the item via its
   *  ref as a side effect. */
  setActiveIndex: (i: number) => void;
  getItemProps: (index: number) => RovingItemProps<T>;
}

/**
 * Returns the lowercase, trimmed prefix used for type-ahead matching.
 * Pulled out for the unit test — pure function, no React.
 */
export function normalizeForTypeahead(s: string): string {
  return s.trim().toLowerCase();
}

/**
 * Type-ahead match: returns the index (>= start) whose label starts with
 * `buffer`, or -1 if none. Wraps around to the start of the list so a
 * user typing past the last match still finds an earlier one. Pulled out
 * as a pure function so it's trivially unit-testable.
 */
export function findTypeaheadMatch(
  labels: ReadonlyArray<string>,
  buffer: string,
  startFrom: number,
): number {
  if (!buffer || labels.length === 0) return -1;
  const needle = normalizeForTypeahead(buffer);
  if (!needle) return -1;
  // Two-pass scan: from `startFrom + 1` to end, then from 0 to `startFrom`.
  // This means a repeated first letter cycles through matches instead of
  // sticking on the first hit — the WAI-ARIA APG behavior.
  //
  // Exception: if the buffer length is 1 ("single letter") we cycle past
  // the current item. If the buffer is longer ("user is still typing"),
  // we include the current item so the existing match stays selected
  // as the buffer extends. This matches the reference menu impl.
  const includeStart = needle.length > 1;
  const n = labels.length;
  const startIdx = includeStart ? startFrom : startFrom + 1;
  for (let i = 0; i < n; i++) {
    const idx = ((startIdx + i) % n + n) % n;
    if (normalizeForTypeahead(labels[idx]).startsWith(needle)) return idx;
  }
  return -1;
}

/**
 * Heuristic: is this key a printable single character we should append
 * to the typeahead buffer? Filters out modifier-bearing key combos
 * (Ctrl+A, Cmd+V, …) and named keys ("Enter", "Tab", …).
 */
function isTypeaheadKey(e: ReactKeyboardEvent): boolean {
  if (e.ctrlKey || e.metaKey || e.altKey) return false;
  // Named keys (length > 1) like "Enter", "ArrowDown" never typeahead.
  // Single-character keys (letters, digits, punctuation, space) do.
  // Space gets special treatment elsewhere — it activates *unless* the
  // typeahead buffer is non-empty (so "j a" still matches "java").
  return e.key.length === 1;
}

export function useRovingTabindex<T extends HTMLElement>(
  opts: UseRovingTabindexOptions,
): UseRovingTabindexResult<T> {
  const {
    itemCount,
    initialIndex = 0,
    loop = true,
    onActivate,
    onEscape,
    getLabel,
    typeaheadTimeoutMs = 500,
  } = opts;

  const [activeIndex, setActiveIndexInternal] = useState(
    Math.min(Math.max(initialIndex, 0), Math.max(itemCount - 1, 0)),
  );

  // Refs to the rendered items. Indexed by position; sparse entries
  // (null) tolerated for items that haven't mounted yet.
  const itemRefs = useRef<Array<T | null>>([]);

  // Type-ahead buffer + reset timer. Kept in refs so they don't trigger
  // re-renders.
  const typeaheadBufferRef = useRef('');
  const typeaheadTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  // Clamp activeIndex if the item count shrinks under us (e.g. a
  // dynamically-registered menu lost an entry). New larger lists keep
  // the current index — no surprise jump.
  useEffect(() => {
    if (itemCount === 0) return;
    if (activeIndex >= itemCount) {
      setActiveIndexInternal(itemCount - 1);
    }
  }, [itemCount, activeIndex]);

  // Side effect: when activeIndex changes (or the matching ref mounts
  // late), focus that item. Skipped when no ref exists at that index —
  // the hook never crashes on a partially-mounted composite.
  useEffect(() => {
    const el = itemRefs.current[activeIndex];
    if (el) el.focus();
  }, [activeIndex]);

  // Public setter — exposed so callers can force focus on open (e.g.
  // AddPanel re-anchoring on the first item when the menu opens).
  // Wraps the internal setter so the focus effect above runs even when
  // the index is set to its current value (re-mount cases).
  const setActiveIndex = useCallback((i: number) => {
    setActiveIndexInternal(i);
    // Explicit imperative focus: covers the "set to same value" path
    // where React's setState bails out before the effect fires.
    const el = itemRefs.current[i];
    if (el) el.focus();
  }, []);

  const clearTypeaheadTimer = useCallback(() => {
    if (typeaheadTimerRef.current !== null) {
      clearTimeout(typeaheadTimerRef.current);
      typeaheadTimerRef.current = null;
    }
  }, []);

  const resetTypeaheadAfterIdle = useCallback(() => {
    clearTypeaheadTimer();
    typeaheadTimerRef.current = setTimeout(() => {
      typeaheadBufferRef.current = '';
      typeaheadTimerRef.current = null;
    }, typeaheadTimeoutMs);
  }, [clearTypeaheadTimer, typeaheadTimeoutMs]);

  // Clean up the timer on unmount so we don't fire setState (via the
  // typeahead clear path) after the hook owner is gone.
  useEffect(() => {
    return () => {
      clearTypeaheadTimer();
    };
  }, [clearTypeaheadTimer]);

  const move = useCallback(
    (delta: number) => {
      if (itemCount === 0) return;
      let next = activeIndex + delta;
      if (loop) {
        next = ((next % itemCount) + itemCount) % itemCount;
      } else {
        next = Math.min(Math.max(next, 0), itemCount - 1);
      }
      setActiveIndexInternal(next);
    },
    [activeIndex, itemCount, loop],
  );

  const handleTypeahead = useCallback(
    (ch: string) => {
      if (!getLabel || itemCount === 0) return false;
      // Build the labels array lazily — small N, called only on
      // printable keystrokes inside the menu. We collect them once per
      // typeahead event rather than memoizing across renders because
      // labels are derived from the caller's registry snapshot.
      const labels: string[] = [];
      for (let i = 0; i < itemCount; i++) labels.push(getLabel(i));
      const nextBuffer = typeaheadBufferRef.current + ch;
      typeaheadBufferRef.current = nextBuffer;
      const match = findTypeaheadMatch(labels, nextBuffer, activeIndex);
      if (match >= 0) {
        setActiveIndexInternal(match);
      }
      resetTypeaheadAfterIdle();
      return true;
    },
    [activeIndex, getLabel, itemCount, resetTypeaheadAfterIdle],
  );

  const onKeyDown = useCallback(
    (e: ReactKeyboardEvent) => {
      switch (e.key) {
        case 'ArrowDown':
          e.preventDefault();
          move(1);
          return;
        case 'ArrowUp':
          e.preventDefault();
          move(-1);
          return;
        case 'Home':
          e.preventDefault();
          if (itemCount > 0) setActiveIndexInternal(0);
          return;
        case 'End':
          e.preventDefault();
          if (itemCount > 0) setActiveIndexInternal(itemCount - 1);
          return;
        case 'Enter':
          e.preventDefault();
          onActivate?.(activeIndex);
          return;
        case 'Escape':
          e.preventDefault();
          onEscape?.();
          return;
        case ' ':
          // Space activates *unless* a typeahead buffer is already in
          // play (in which case it extends the buffer — "j a v" matches
          // "java"). Matches WAI-ARIA APG.
          if (typeaheadBufferRef.current.length > 0 && getLabel) {
            e.preventDefault();
            handleTypeahead(' ');
            return;
          }
          e.preventDefault();
          onActivate?.(activeIndex);
          return;
        default:
          if (isTypeaheadKey(e) && getLabel) {
            // Don't preventDefault printable keys outside of the
            // typeahead path — but here, typeahead claims them.
            e.preventDefault();
            handleTypeahead(e.key);
          }
          return;
      }
    },
    [activeIndex, getLabel, handleTypeahead, itemCount, move, onActivate, onEscape],
  );

  const getItemProps = useCallback(
    (index: number): RovingItemProps<T> => ({
      ref: (el: T | null) => {
        itemRefs.current[index] = el;
        // If this is the active index and the ref *just* mounted, focus
        // it now. Handles the "menu opens with initialIndex=0" case
        // where the effect above fires before the ref is attached.
        if (el && index === activeIndex) {
          // Defer to next tick so React finishes attaching first.
          // Without this, focus runs before the element is in layout
          // and Chrome rejects the call silently.
          queueMicrotask(() => {
            // Re-check: the ref may have been replaced or unmounted
            // between the schedule and the microtask flush.
            if (itemRefs.current[index] === el && document.contains(el)) {
              el.focus();
            }
          });
        }
      },
      tabIndex: index === activeIndex ? 0 : -1,
      onKeyDown,
    }),
    [activeIndex, onKeyDown],
  );

  return { activeIndex, setActiveIndex, getItemProps };
}
