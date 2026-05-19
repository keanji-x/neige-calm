// Resolves the "Today terminal" — a singleton scratch PTY mounted on the
// home page so the user always lands in a live shell. Strategy:
//
//   1. Read `calm.todayCardId` from localStorage.
//   2. If present, GET /api/cards/:id/terminal to validate the card still
//      has a terminal row. Returns `{cardId, terminalId}` on success.
//   3. On miss / 404 / network fail: bootstrap a fresh one inside a hidden
//      "Scratch" cove + "Today" wave (lazily created if absent), then store
//      the new cardId.
//
// Browser-scoped (not per-user) by design — auth and per-user state come
// with M3. Clearing site data costs you the binding; the underlying
// Terminal row stays in the Scratch cove until you delete the card.

import { useCallback, useEffect, useRef, useState } from 'react';
import * as api from '../api/calm';

const STORAGE_KEY = 'calm.todayCardId';
const SCRATCH_COVE_NAME = 'Scratch';
const SCRATCH_COVE_COLOR = '#6a8';
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
      //    same Scratch cove, same Today wave, same first terminal card
      //    (across browsers / cleared-storage cycles) so the kernel doesn't
      //    accumulate orphan cards.
      const cove = await ensureScratchCove();
      const wave = await ensureTodayWave(cove.id);
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

      const card = await api.createCard(wave.id, { kind: 'terminal' });
      const term = await api.createTerminal(card.id, {});
      const patched = await api.updateCard(card.id, {
        payload: { terminal_id: term.id },
      });
      localStorage.setItem(STORAGE_KEY, patched.id);
      setToday({ cardId: patched.id, terminalId: term.id });
    } catch (e) {
      setError(e as Error);
    } finally {
      inFlightRef.current = false;
    }
  }, []);

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

async function ensureScratchCove() {
  const coves = await api.listCoves();
  const existing = coves.find((c) => c.name === SCRATCH_COVE_NAME);
  if (existing) return existing;
  return api.createCove({ name: SCRATCH_COVE_NAME, color: SCRATCH_COVE_COLOR });
}

async function ensureTodayWave(coveId: string) {
  const waves = await api.wavesInCove(coveId);
  const existing = waves.find((w) => w.title === TODAY_WAVE_TITLE);
  if (existing) return existing;
  return api.createWave({ cove_id: coveId, title: TODAY_WAVE_TITLE });
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
