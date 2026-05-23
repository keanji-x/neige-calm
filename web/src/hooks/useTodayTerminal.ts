// Resolves the "Today terminal" — a singleton kernel-owned Today PTY
// mounted on the home page so the user always lands in a live shell.
// Lives inside the hidden system cove (issue #175) — the sidebar never
// renders it, but the same Terminal row backs every browser tab. Strategy:
//
//   1. Read `calm.todayCardId` from localStorage.
//   2. If present, GET /api/cards/:id/terminal to validate the card still
//      has a terminal row. Returns `{cardId, terminalId}` on success.
//   3. On miss / 404 / network fail: bootstrap a fresh one inside the
//      kernel-owned **system cove** (issue #175 — hidden from the
//      sidebar, lookup via `POST /api/coves/system`), hosting a single
//      internal "Today" wave + terminal card.
//
// Browser-scoped (not per-user) by design — auth and per-user state come
// with M3. Clearing site data costs you the binding; the underlying
// Terminal row stays in the system cove until you delete the card.
//
// This hook is the one place in the app that performs an imperative
// bootstrap sequence rather than a single mutation. We could decompose
// it into `useCreateCoveMutation` etc., but the "ensureSystemCove → ensure
// TodayWave → ensureTerminalCard" chain is read-then-write three times
// over, and modeling it as a single async resolver keeps the idempotency
// invariants in one place. After mutating, we invalidate the affected
// query keys so other consumers (Sidebar, Cove page) see the new rows.

import { useCallback, useEffect, useRef } from 'react';
import { useState } from '../shared/state';
import { useQueryClient } from '@tanstack/react-query';
import * as api from '../api/calm';
import { DARK_THEME_RGB } from '../api/themeRgb';
import { queryKeys } from '../api/queries';

const STORAGE_KEY = 'calm.todayCardId';
// Internal wave title inside the system cove. The user never sees this —
// `GET /api/coves` filters the system cove out by default (kind='system'),
// so the only consumer is this hook's `ensureTodayWave` lookup. The label
// can stay human-readable for debugging without colliding with anything
// the user names a wave (different cove, no name collision possible).
const TODAY_WAVE_TITLE = 'Today';

export interface TodayTerminal {
  cardId: string;
  terminalId: string;
}

export interface UseTodayTerminalResult {
  /** `null` while we're resolving or bootstrapping. */
  today: TodayTerminal | null;
  error: Error | null;
  /** Wipe the binding and force a re-bootstrap. Useful when the PTY's WS
   *  closes immediately, suggesting the daemon is gone behind the stored id. */
  reset: () => void;
}

export function useTodayTerminal(): UseTodayTerminalResult {
  const [today, setToday] = useState<TodayTerminal | null>(null);
  const [error, setError] = useState<Error | null>(null);
  const inFlightRef = useRef(false);
  const qc = useQueryClient();

  const resolve = useCallback(async () => {
    if (inFlightRef.current) return;
    inFlightRef.current = true;
    try {
      // 1. Fast path: cached cardId still resolves.
      const cached = typeof localStorage !== 'undefined'
        ? localStorage.getItem(STORAGE_KEY)
        : null;
      if (cached) {
        try {
          const term = await api.getTerminalForCard(cached);
          setToday({ cardId: cached, terminalId: term.id });
          return;
        } catch (e: unknown) {
          // 404 → fall through to bootstrap. Other errors propagate so
          // the user sees something is wrong rather than a silent reset.
          if (!isNotFound(e)) {
            setError(e as Error);
            return;
          }
          // Stale binding — clear and re-bootstrap.
          localStorage.removeItem(STORAGE_KEY);
        }
      }

      // 2. Bootstrap path. Reuse existing infra where possible:
      //    same system cove (singleton enforced by the kernel — issue
      //    #175), same Today wave, same first terminal card (across
      //    browsers / cleared-storage cycles) so the kernel doesn't
      //    accumulate orphan cards.
      //
      //    Per #175 we cheaply invalidate the coves query unconditionally
      //    rather than tracking a "created" flag — the system cove is
      //    filtered out of the user-facing list by default, so the
      //    invalidation is a no-op cache refresh in the common case and
      //    not worth the round-trip parsing to gate.
      const cove = await api.getOrCreateSystemCove();
      void qc.invalidateQueries({ queryKey: queryKeys.coves() });
      const { wave, created: waveCreated } = await ensureTodayWave(cove.id);
      if (waveCreated) {
        void qc.invalidateQueries({ queryKey: queryKeys.wavesInCove(cove.id) });
      }
      const detail = await api.getWaveDetail(wave.id);
      const existingCard = detail.cards.find((c) => {
        if (c.kind !== 'terminal') return false;
        const p = c.payload as { terminal_id?: string } | null;
        return typeof p?.terminal_id === 'string';
      });
      if (existingCard) {
        const tid = (existingCard.payload as { terminal_id: string }).terminal_id;
        // Validate the terminal row still exists.
        try {
          await api.getTerminalForCard(existingCard.id);
          localStorage.setItem(STORAGE_KEY, existingCard.id);
          setToday({ cardId: existingCard.id, terminalId: tid });
          return;
        } catch {
          // Stale card (terminal was reaped). Fall through to fresh create.
        }
      }

      // Atomic create (#13) — one round-trip writes the card row, the
      // linked terminal row, AND spawns the daemon. The kernel stamps the
      // `schemaVersion` + `terminal_id` payload itself, and a single
      // `card.added` event drives the cache invalidate via EventBridge.
      const card = await api.createTerminalCard(wave.id, {
        // #177 — placeholder until PR4 wires the real host theme read.
        theme: DARK_THEME_RGB,
      });
      const terminalId = (card.payload as { terminal_id: string }).terminal_id;
      localStorage.setItem(STORAGE_KEY, card.id);
      setToday({ cardId: card.id, terminalId });
    } catch (e) {
      setError(e as Error);
    } finally {
      inFlightRef.current = false;
    }
  }, [qc]);

  useEffect(() => {
    void resolve();
  }, [resolve]);

  const reset = useCallback(() => {
    try {
      localStorage.removeItem(STORAGE_KEY);
    } catch {
      /* private mode etc. — best effort */
    }
    setToday(null);
    setError(null);
    void resolve();
  }, [resolve]);

  return { today, error, reset };
}

// ---------------------------------------------------------------------------

/**
 * Look up the single "Today" wave inside the kernel-owned system cove,
 * minting it if absent. Identifying the wave by `title === 'Today'`
 * inside the system cove is safe because the user can't reach this cove
 * — `GET /api/coves` filters it out by default (issue #175) and the
 * sidebar's "+ New wave" affordance always targets a user-visible cove.
 * No collision risk with whatever a user names their own waves.
 */
async function ensureTodayWave(coveId: string) {
  const waves = await api.wavesInCove(coveId);
  const existing = waves.find((w) => w.title === TODAY_WAVE_TITLE);
  if (existing) return { wave: existing, created: false };
  // Issue #250 PR 2 — `createWave` now requires `cwd`. The Today
  // wave is a kernel-internal singleton inside the system cove (a
  // cove the user never sees); its spec daemon doesn't need a
  // meaningful project cwd. We pass `/` as a placeholder. The
  // server detects `cove.kind == System` and exempts this row from
  // the cove_folders claim namespace, so `attach_folder` is a no-op
  // here (the cwd is just stored on the wave row for the daemon's
  // chdir). A subsequent call into this helper finds the existing
  // wave and never re-mints (the `existing` short-circuit above).
  const wave = await api.createWave({
    cove_id: coveId,
    title: TODAY_WAVE_TITLE,
    cwd: '/',
    attach_folder: false,
    // #177 — placeholder until PR4 wires the real host theme read.
    theme: DARK_THEME_RGB,
  });
  return { wave, created: true };
}

function isNotFound(e: unknown): boolean {
  // `CalmApiError` shape, defensive-checked so we don't import the class
  // (avoids a `instanceof` mismatch under React Fast Refresh).
  return (
    typeof e === 'object' &&
    e !== null &&
    'status' in e &&
    (e as { status: unknown }).status === 404
  );
}
