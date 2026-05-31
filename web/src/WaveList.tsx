// WaveList — the keyboard-canonical alternative to WaveGrid.
//
// Slice 9 of issue #56 (Path C). The grid view (`WaveGrid.tsx`) is mouse-
// only for layout changes — RGL has no keyboard story, and the
// `card-drag-handle` is a `<div>` with no activation path. Rather than
// retrofit keyboard semantics into RGL (Path A: deferred indefinitely), we
// ship a separate list-view component that's first-class keyboard-driven.
//
// What this implements
// --------------------
//
//   - Cards laid out as a semantic `<ul>` with `<li>` wrappers; each `<li>`
//     hosts the same `<WaveCard>` the grid view renders inside its
//     `wave-card`. The list owns no positions — order is the kernel-
//     canonical `card.sort` value, identical to what the wave-detail
//     query already returns.
//
//   - Roving tabindex (`useRovingTabindex`, Slice 7) over the `<li>` items:
//     only the active item has `tabIndex={0}`; ArrowUp / ArrowDown move
//     focus; Home / End jump to first / last. The Tab order from outside
//     enters the list at the active item exactly once — pressing Tab
//     again exits to whatever comes after.
//
//   - **Alt+ArrowUp / Alt+ArrowDown** reorder the focused card by swapping
//     `sort` values with the previous / next card via the existing
//     `useUpdateCardMutation` (which has an "optimistic for sort" path).
//     The trace ring buffer picks up the resulting `card.updated` events
//     naturally — no special wiring.
//
//   - **Delete** removes the focused card. Matches the grid view's `×`
//     button behavior (the existing `onRemoveCard(idx)` flow); grid view
//     does not confirm, so list view doesn't either.
//
// Why a separate component (not a flag inside WaveGrid)
// ------------------------------------------------------
//
// WaveGrid carries ~440 LOC of RGL-specific layout reconciliation,
// localStorage migration, and rAF-coalesced overlay writes. Conditionally
// rendering a totally different DOM shape (`<ul>` vs RGL's positioned
// children) from inside that component would mean dragging all of that
// logic past the conditional. A sibling component is cleaner: each file
// stays focused on one model. `pages/Wave.tsx` picks between them based on
// the view-mode overlay.

import { useCallback, useMemo, useRef } from 'react';
import { useRovingTabindex } from './ui/hooks/useRovingTabindex';
import { CloseIcon } from './shared/components/CloseIcon';
import { WaveCard } from './shared/components/WaveCard';
import { UnknownCard } from './cards/UnknownCard';
import { dlog } from './util/debug';
import { useUpdateCardMutation } from './api/queries';
import type { WaveCardSlot } from './types';
import { handleWheelCardPointerDown } from './input/cardShell';

// Card identity for keys. Mirrors `WaveGrid.slotKey` — we share the same
// stable key shape so list/grid toggles don't unmount and remount cards
// just because the wrapper changed. (xterm sessions stay alive across the
// toggle; codex hooks continue to land on the same component instance.)
function slotKey(slot: WaveCardSlot, idx: number): string {
  if (slot.kind === 'card') return slot.card.id ?? `idx-${idx}`;
  return slot.id || `idx-${idx}`;
}

// Best-effort accessible name for a card row. Used by `aria-label` on the
// `<li>` so screen readers and Playwright `getByRole('listitem', { name })`
// queries land on a meaningful string. Falls back to a stable "card N"
// placeholder when no per-kind title is available.
function slotAccessibleName(slot: WaveCardSlot, idx: number): string {
  if (slot.kind === 'unknown') {
    return `Unknown card: ${slot.kernelKind}`;
  }
  const card = slot.card;
  if (card.type === 'terminal') {
    return card.title ? `Terminal: ${card.title}` : 'Terminal';
  }
  if (card.type === 'codex') return 'Codex';
  if (card.type === 'plugin') return `Plugin: ${card.resource_uri}`;
  // Fallback for future kinds — the index identifies the row uniquely
  // within the wave so screen-reader users can still address it.
  return `Card ${idx + 1}`;
}

// Card id for the update mutation. The kernel addresses cards by id; list-
// view only reorders cards that have a real id (the auto-packed positional
// fallback in the grid is for layout coords, not kernel identity). When
// the id is missing — happens for in-flight optimistic adds before the
// kernel acks — Alt+ArrowUp/Down is a no-op.
function slotCardId(slot: WaveCardSlot): string | null {
  if (slot.kind === 'unknown') return slot.id;
  return slot.card.id ?? null;
}

export function WaveList({
  waveId,
  cards,
  onRemoveCard,
}: {
  waveId: string;
  cards: WaveCardSlot[];
  onRemoveCard: (idx: number) => void;
}) {
  dlog('WaveList', 'render', { waveId, cardsCount: cards.length });

  const updateCard = useUpdateCardMutation();

  // The list view is the kernel's canonical ordering: we mirror the wave
  // detail's `cards` array, which the server returns sorted by `sort`
  // ascending. (`router.tsx` passes `detail.cards` straight through;
  // reorders land via WS `card.updated` events and re-fetch / re-sort
  // server-side. So consuming the prop's order directly is correct.)
  const items = useMemo(() => cards, [cards]);

  // Roving tabindex over the list items. We treat the `<li>` itself as
  // the focusable element so Tab into the list lands on the active card
  // wrapper, not its first inner control — which matches WAI-ARIA's
  // listbox / grid patterns and lines up with the AT story documented
  // in the a11y contract.
  //
  // `onActivate` is omitted intentionally: the natural activation for a
  // list item here is "enter the card to drive its inner UI", which is
  // simply the next Tab keystroke. Mapping Enter to a synthetic activate
  // would conflict with terminal cards that need raw key passes.
  const { activeIndex, getItemProps } = useRovingTabindex<HTMLLIElement>({
    itemCount: items.length,
  });

  // Ref the active card id at swap time so the mutation that runs across
  // a re-render still references the same card. Without this, a slow
  // server ack could let `activeIndex` drift before the user's second
  // Alt+Down lands; the swap would then operate on a stale card.
  const swapInFlight = useRef(false);

  const swap = useCallback(
    (from: number, to: number) => {
      if (from === to) return;
      if (from < 0 || to < 0 || from >= items.length || to >= items.length) return;
      const a = items[from];
      const b = items[to];
      const aId = slotCardId(a);
      const bId = slotCardId(b);
      // Both must have kernel ids to be reorderable. Slots created
      // optimistically before the server ack carry no id; we don't
      // attempt to reorder them.
      if (!aId || !bId) return;
      // Both must have a `sort` value. The router populates it from
      // `KernelCard.sort` (always present for server-resident cards),
      // but defensive against missing-on-read transitions.
      if (a.sort == null || b.sort == null) return;
      if (swapInFlight.current) return;
      swapInFlight.current = true;
      const aSort = a.sort;
      const bSort = b.sort;
      dlog('WaveList', 'swap', {
        from,
        to,
        aId,
        bId,
        aSort,
        bSort,
      });
      // Two mutations, run sequentially: each card gets the other's
      // `sort`. The useUpdateCardMutation hook is optimistic for
      // `sort`, so the list re-renders with the swapped order before
      // the server acks. Sequential (not Promise.all) so the second
      // mutate's `onMutate` reads the cache *after* the first's
      // optimistic write — preventing the second's snapshot from
      // shadowing the first's update.
      //
      // If the first succeeds and the second fails, the wave ends up
      // with two cards sharing one sort and the other unset. The
      // refetch reconciles via the kernel's order; a re-press of
      // Alt+ArrowUp restores the prior order. We log but don't
      // surface a toast — same pattern as the existing rename / drag
      // error paths.
      void (async () => {
        try {
          await updateCard.mutateAsync({ id: aId, body: { sort: bSort } });
          await updateCard.mutateAsync({ id: bId, body: { sort: aSort } });
        } catch (err) {
          // eslint-disable-next-line no-console
          console.warn('[WaveList] sort swap failed', err);
        } finally {
          swapInFlight.current = false;
        }
      })();
    },
    [items, updateCard],
  );

  return (
    <div className="wave-list-wrap">
      <ul
        className="wave-list"
        aria-label="Wave cards (list view)"
      >
        {items.map((slot, i) => {
          const key = slotKey(slot, i);
          const name = slotAccessibleName(slot, i);
          const props = getItemProps(i);
          // We need to extend the roving-tabindex onKeyDown with the
          // list-specific Alt+ArrowUp/Down + Delete handling. The hook
          // already handles ArrowUp/Down/Home/End/Enter/Space/Escape;
          // we keep its handler for those and only branch on the keys
          // it doesn't claim.
          const onKeyDown = (e: React.KeyboardEvent<HTMLLIElement>) => {
            // Alt+ArrowUp: swap with previous (move card up in list).
            if (e.altKey && e.key === 'ArrowUp') {
              e.preventDefault();
              swap(i, i - 1);
              return;
            }
            // Alt+ArrowDown: swap with next (move card down in list).
            if (e.altKey && e.key === 'ArrowDown') {
              e.preventDefault();
              swap(i, i + 1);
              return;
            }
            // Delete / Backspace: remove the focused card. Mirrors the
            // grid view's `×` button (no confirmation — grid view
            // doesn't confirm, so list view doesn't either).
            if (e.key === 'Delete' || e.key === 'Backspace') {
              e.preventDefault();
              onRemoveCard(i);
              return;
            }
            // Defer to the hook for ArrowUp/Down/Home/End/Enter/Space.
            // Non-Alt arrows fall through here; the hook owns them.
            // `useRovingTabindex` will preventDefault when it claims a
            // key.
            props.onKeyDown(e);
          };
          return (
            // eslint-disable-next-line jsx-a11y/no-noninteractive-element-interactions
            <li
              key={key}
              ref={props.ref}
              tabIndex={props.tabIndex}
              data-wheel-card
              onPointerDownCapture={handleWheelCardPointerDown}
              onKeyDown={onKeyDown}
              aria-label={name}
              aria-posinset={i + 1}
              aria-setsize={items.length}
              aria-keyshortcuts="ArrowUp ArrowDown Alt+ArrowUp Alt+ArrowDown Home End Delete"
              className={
                'wave-list-item' + (i === activeIndex ? ' is-active' : '')
              }
              data-card-id={slotCardId(slot) ?? undefined}
            >
              <button
                type="button"
                className="card-list-close"
                onClick={(e) => {
                  e.stopPropagation();
                  onRemoveCard(i);
                }}
                title="Remove panel"
                aria-label={`Remove ${name}`}
              >
                <CloseIcon />
              </button>
              {slot.kind === 'card' ? (
                <WaveCard card={slot.card} deletable={slot.deletable} />
              ) : (
                <UnknownCard kernelKind={slot.kernelKind} />
              )}
            </li>
          );
        })}
      </ul>
      {items.length === 0 && (
        <p className="wave-list-empty">No cards in this wave yet.</p>
      )}
    </div>
  );
}
