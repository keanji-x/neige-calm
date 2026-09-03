// Resolves the "Today terminal" — a singleton kernel-owned Today PTY
// mounted on the home page so the user always lands in a live shell.
// Lives inside the hidden system area (issue #175) — the sidebar never
// renders it, but the same Terminal row backs every browser tab. Strategy:
//
//   1. Read `calm.todayCardId` from localStorage.
//   2. If present, GET /api/cards/:id/terminal to validate the card still
//      has a terminal row. Returns `{cardId, terminalId}` on success.
//   3. On miss / 404 / network fail: bootstrap a fresh one inside the
//      kernel-owned **system area** (issue #175 — hidden from the
//      sidebar, lookup via `POST /api/areas/system`), hosting a single
//      internal "Today" wave + terminal card.
//
// Browser-scoped (not per-user) by design — auth and per-user state come
// with M3. Clearing site data costs you the binding; the underlying
// Terminal row stays in the system area until you delete the card.
//
// This hook is the one place in the app that performs an imperative
// bootstrap sequence rather than a single mutation. We could decompose
// it into `useCreateAreaMutation` etc., but the "ensureSystemArea → ensure
// TodayWave → ensureTerminalCard" chain is read-then-write three times
// over, and modeling it as a single async resolver keeps the idempotency
// invariants in one place. After mutating, we invalidate the affected
// query keys so other consumers (Sidebar, Area page) see the new rows.

import { useCallback, useEffect, useRef } from 'react';
import { useState } from '../shared/state';
import { useQueryClient } from '@tanstack/react-query';
import * as api from '../api/calm';
import { DARK_THEME_RGB, LIGHT_THEME_RGB } from '../api/themeRgb';
import { queryKeys } from '../api/queries';

const STORAGE_KEY = 'calm.todayCardId';
// Internal wave title inside the system area. The user never sees this —
// `GET /api/areas` filters the system area out by default (kind='system'),
// so the only consumer is this hook's `ensureTodayWave` lookup. The label
// can stay human-readable for debugging without colliding with anything
// the user names a wave (different area, no name collision possible).
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
      //    same system area (singleton enforced by the kernel — issue
      //    #175), same Today wave, same first terminal card (across
      //    browsers / cleared-storage cycles) so the kernel doesn't
      //    accumulate orphan cards.
      //
      //    Per #175 we cheaply invalidate the areas query unconditionally
      //    rather than tracking a "created" flag — the system area is
      //    filtered out of the user-facing list by default, so the
      //    invalidation is a no-op cache refresh in the common case and
      //    not worth the round-trip parsing to gate.
      const area = await api.getOrCreateSystemArea();
      void qc.invalidateQueries({ queryKey: queryKeys.areas() });
      const { wave, created: waveCreated } = await ensureTodayWave(area.id);
      if (waveCreated) {
        void qc.invalidateQueries({ queryKey: queryKeys.wavesInArea(area.id) });
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
      //
      // #177 — pass host browser theme so the Today terminal's daemon
      // answers codex's OSC 10/11 probe with matching colors. Read from
      // `<html data-theme>` (the ThemeProvider's synchronous mirror)
      // rather than via `useTheme()` so this hook stays cheap and
      // doesn't re-render on theme toggle.
      const card = await api.createTerminalCard(wave.id, {
        theme: readHostThemeRgb(),
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
 * Look up the single "Today" wave inside the kernel-owned system area,
 * minting it if absent. Identifying the wave by `title === 'Today'`
 * inside the system area is safe because the user can't reach this area
 * — `GET /api/areas` filters it out by default (issue #175) and the
 * sidebar's "+ New wave" affordance always targets a user-visible area.
 * No collision risk with whatever a user names their own waves.
 */
async function ensureTodayWave(areaId: string) {
  const waves = await api.wavesInArea(areaId);
  const existing = waves.find((w) => w.title === TODAY_WAVE_TITLE);
  if (existing) return { wave: existing, created: false };
  // #1147 S3 — `cwd` is OMITTED, and that is the fix, not a shortcut.
  //
  // This used to send `cwd: '/'` as a placeholder, on the reasoning that a
  // kernel-internal wave's spec daemon "doesn't need a meaningful project
  // cwd". Two things have changed since:
  //
  //  * Since #1131/S2 an omitted `cwd` is the *managed* branch: the kernel
  //    allocates `<workspace-root>/<area>/<wave>`, creates it, `git init`s it
  //    and owns it. That is a directory a worker can actually lease — `/` was
  //    never one, so any `kind: codex` task on this wave died in
  //    `git_repo_root_for_wave_cwd` with nothing but `spawn-failed`. That is
  //    the defect #1147 was opened on, and this placeholder was one of its
  //    live sources.
  //  * S3 makes an explicit `cwd` mean "attach this existing repository", and
  //    validates it (absolute, exists, inside a Git work tree). `/` fails that
  //    check by construction, so continuing to send it would 400 the Today
  //    bootstrap outright.
  //
  // `attach_folder` goes with it: with no cwd there is nothing to claim, and
  // the system area was exempt from the `area_folders` namespace anyway.
  // A subsequent call into this helper finds the existing wave and never
  // re-mints (the `existing` short-circuit above).
  const wave = await api.createWave({
    area_id: areaId,
    title: TODAY_WAVE_TITLE,
    // #177 — same `readHostThemeRgb()` source as the terminal-card
    // create below. The spec daemon that the wave-create txn spawns
    // gets matching colors on its first paint.
    theme: readHostThemeRgb(),
  });
  return { wave, created: true };
}

/**
 * Read the current host theme from `<html data-theme>` (written
 * synchronously by `ThemeProvider`) and return the matching RGB
 * tuple. Default → dark when `document` is unavailable (SSR / test
 * environments where the provider hasn't mounted yet); this mirrors
 * the server-side `RequestTheme::default_dark()` sentinel so a
 * pre-provider read can't crash and lands on a defensible value.
 */
function readHostThemeRgb() {
  if (typeof document === 'undefined') return DARK_THEME_RGB;
  return document.documentElement.dataset.theme === 'light'
    ? LIGHT_THEME_RGB
    : DARK_THEME_RGB;
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
