import { describe, expect, it } from 'vitest';

import type { CardEntry } from '../registry.js';
import { createCardRegistry } from '../registry.js';
import type { CodexCard } from './codex.ts';
import { CODEX_CARD_ENTRY, isPlainChatPayload } from './codex.ts';
import { partitionTrackCards } from './headless-filter.js';
import { registerAvailableBuiltinCards } from './register.js';

function wire(id: string, kind: string, payload: unknown) {
  return {
    id, kind, track_id: 'w1', title: null, sort: 0, payload,
    deletable: true, created_at: 0, updated_at: 0,
  };
}

describe('CODEX_CARD_ENTRY', () => {
  it('resolves kernel codex cards, including before terminal_id is projected', () => {
    expect(CODEX_CARD_ENTRY.fromKernel?.({
      id: 'x1', kind: 'codex', payload: { terminal_id: 't1' },
    })).toEqual({ type: 'codex', id: 'x1', title: null, terminalId: 't1', sessionState: null, cwd: null, gateCwd: null });
    // The kernel projects `terminal_id` on read; a card observed between mint and projection resolves with a null terminal rather than not at all.
    expect(CODEX_CARD_ENTRY.fromKernel?.({
      id: 'x2', kind: 'codex', payload: { goal: 'do the thing' },
    })).toEqual({ type: 'codex', id: 'x2', title: null, terminalId: null, sessionState: null, cwd: null, gateCwd: null });
    expect(CODEX_CARD_ENTRY.fromKernel?.({
      id: 'x3', kind: 'terminal', payload: { terminal_id: 't1' },
    })).toBeNull();
    expect(CODEX_CARD_ENTRY.fromKernel?.({
      id: 'x4', kind: 'claude', payload: { terminal_id: 't1' },
    })).toBeNull();
  });

  /* Kind `'codex'` mints three different cards; the payload's `planner_harness` bit is the only thing separating a harness from an ordinary worker. The refusal is the mechanism, not registration order. */
  it('[INV-CARD-180] refuses planner harness payloads so they fall through to planner', () => {
    expect(CODEX_CARD_ENTRY.fromKernel?.({
      id: 's1', kind: 'codex', payload: { planner_harness: true },
    })).toBeNull();
    const registry = createCardRegistry();
    registerAvailableBuiltinCards(registry);
    expect(
      registry.resolve({ id: 's1', kind: 'codex', payload: { planner_harness: true } })?.type,
      'a planner harness card must still resolve as planner through the production registry',
    ).toBe('planner');
    expect(registry.get('planner')?.headless).toBe(true);
    expect(registry.resolve({ id: 'x1', kind: 'codex', payload: { terminal_id: 't1' } })?.type)
      .toBe('codex');
  });

  it('reads only the exact discriminator, not any truthy planner_harness', () => {
    // `isPlannerHarnessPayload` is `=== true`; sharing it with planner keeps the two entries from disagreeing.
    for (const payload of [{ planner_harness: false }, { planner_harness: 'true' }, { planner_harness: 1 }, null, 'x']) {
      expect(CODEX_CARD_ENTRY.fromKernel?.({ id: 'p', kind: 'codex', payload })?.type).toBe('codex');
    }
  });

  /* The third shape under kind `'codex'`: an area plain-chat card carries `harness_profile: "plain_chat"` and has no PTY, so claiming it would render `TerminalCardView` with a null terminal forever. */
  it('[INV-CHAT-016] refuses area plain-chat cards, which have no PTY to render', () => {
    expect(CODEX_CARD_ENTRY.fromKernel?.({
      id: 'chat1', kind: 'codex', payload: { schemaVersion: 1, harness_profile: 'plain_chat' },
    })).toBeNull();
    const registry = createCardRegistry();
    registerAvailableBuiltinCards(registry);
    expect(registry.resolve({
      id: 'chat1', kind: 'codex', payload: { schemaVersion: 1, harness_profile: 'plain_chat' },
    })).toBeNull();
    const { visible, unknown } = partitionTrackCards(registry, [
      wire('chat1', 'codex', { schemaVersion: 1, harness_profile: 'plain_chat' }),
      wire('k-codex', 'codex', { terminal_id: 't1' }),
    ]);
    expect(visible.map((slot) => slot.wire.id)).toEqual(['k-codex']);
    expect(unknown.map((slot) => slot.wire.id)).toEqual(['chat1']);
  });

  it('[INV-CHAT-016] reads only the exact plain-chat marker', () => {
    // Mirrors the kernel's `payload.get("harness_profile").and_then(as_str) == Some("plain_chat")`.
    expect(isPlainChatPayload({ harness_profile: 'plain_chat' })).toBe(true);
    for (const payload of [
      {}, { harness_profile: 'other_profile' }, { harness_profile: true },
      { harness_profile: 1 }, { harness_profile: null }, { harness_profile: {} },
      null, 'x', 7, undefined,
    ]) {
      expect(isPlainChatPayload(payload), `${JSON.stringify(payload)} is not the marker`).toBe(false);
      expect(CODEX_CARD_ENTRY.fromKernel?.({ id: 'p', kind: 'codex', payload })?.type).toBe('codex');
    }
  });

  /* The no-claim rule: an exact claim here would still resolve harnesses as `planner` (resolve falls through on `null`), so only this assertion catches adding one. Read through the interface: `satisfies` narrows the constant's own type. */
  it('[INV-CARD-180] takes no claim on the shared kernel kind', () => {
    expect((CODEX_CARD_ENTRY as CardEntry<CodexCard>).claim).toBeUndefined();
  });

  it('is user-creatable through the kind\'s own atomic endpoint', () => {
    expect(CODEX_CARD_ENTRY.create.mode).toBe('atomic');
  });

  it('offers a title and a working directory, and nothing about codex itself', () => {
    /* Read through the interface: `satisfies` narrows the constant's own type and would make these assertions tautologies. */
    const addPanel = (CODEX_CARD_ENTRY as CardEntry<CodexCard>).addPanel;
    expect(addPanel?.label).toBe('codex');
    expect(addPanel?.fields?.map((field) => [field.key, field.kind]))
      .toEqual([['title', 'text'], ['cwd', 'directory']]);
    expect(addPanel?.fields?.some((field) => field.required === true)).toBe(false);
  });

  it('registers as a surface-owning built-in', () => {
    const registry = createCardRegistry();
    registerAvailableBuiltinCards(registry);
    expect(registry.get('codex')?.headless).toBe(false);
    expect(registry.resolve({ id: 'x1', kind: 'codex', payload: { terminal_id: 't9' } })?.type)
      .toBe('codex');
  });

  it('lands codex cards in the visible partition beside terminal cards', () => {
    const registry = createCardRegistry();
    registerAvailableBuiltinCards(registry);
    const { visible, unknown } = partitionTrackCards(registry, [
      wire('k-codex', 'codex', { terminal_id: 't1' }),
      wire('k-term', 'terminal', { terminal_id: 't2' }),
      wire('k-planner', 'codex', { planner_harness: true }),
    ]);
    expect(visible.map((slot) => slot.card.type)).toEqual(['codex', 'terminal']);
    expect(unknown).toEqual([]);
  });
});
