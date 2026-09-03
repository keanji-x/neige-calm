import { describe, expect, it } from 'vitest';

import type { CardEntry } from '../registry.js';
import { createCardRegistry } from '../registry.js';
import type { AssistantCard } from './assistant.js';
import { ASSISTANT_CARD_ENTRY, isAssistantHarnessPayload } from './assistant.js';
import { registerAvailableBuiltinCards } from './register.js';

const callComponent = (entry: typeof ASSISTANT_CARD_ENTRY) =>
  (entry.component as unknown as (props: unknown) => unknown)({});

describe('assistant card entry', () => {
  it('recognises a track assistant only by the assistant marker', () => {
    expect(ASSISTANT_CARD_ENTRY.fromKernel?.({
      id: 'c1', kind: 'codex', payload: { harness_profile: 'assistant' },
    })).toEqual({ type: 'assistant', id: 'c1' });
    expect(ASSISTANT_CARD_ENTRY.fromKernel?.({
      id: 'c2', kind: 'codex', payload: { harness_profile: 'assistant', anything: 1 },
    })).toEqual({ type: 'assistant', id: 'c2' });
  });

  /*
   * The two markers live under one field, and this is the direction that
   * actually costs something: a predicate widened to "has a `harness_profile`"
   * would make every area chat card resolve as a track assistant. The server
   * refuses to conflate them for the same reason and pins it in
   * `plain_chat.rs::the_two_conversation_markers_never_answer_for_each_other`.
   */
  it('refuses the plain-chat marker and an unmarked codex card', () => {
    for (const payload of [
      {}, { harness_profile: 'plain_chat' }, { harness_profile: true },
      { harness_profile: 'Assistant' }, { spec_harness: true },
    ]) {
      expect(
        ASSISTANT_CARD_ENTRY.fromKernel?.({ id: 'c', kind: 'codex', payload }),
        JSON.stringify(payload),
      ).toBeNull();
    }
  });

  it('narrows non-object payloads instead of throwing', () => {
    for (const payload of [null, undefined, 'assistant', 7, true, []]) {
      expect(() => ASSISTANT_CARD_ENTRY.fromKernel?.({ id: 'c', kind: 'codex', payload })).not.toThrow();
      expect(ASSISTANT_CARD_ENTRY.fromKernel?.({ id: 'c', kind: 'codex', payload })).toBeNull();
    }
    expect(isAssistantHarnessPayload(null)).toBe(false);
    expect(isAssistantHarnessPayload({ harness_profile: 'assistant' })).toBe(true);
  });

  it('never claims a kernel kind other than codex', () => {
    for (const kind of ['terminal', 'claude', 'assistant']) {
      expect(ASSISTANT_CARD_ENTRY.fromKernel?.({
        id: 'c', kind, payload: { harness_profile: 'assistant' },
      })).toBeNull();
    }
  });

  it('is headless, 1x1 and kernel-minted only', () => {
    expect(callComponent(ASSISTANT_CARD_ENTRY)).toBeNull();
    expect(ASSISTANT_CARD_ENTRY.headless).toBe(true);
    expect(ASSISTANT_CARD_ENTRY.defaultSize).toEqual({ w: 1, h: 1, minW: 1, minH: 1 });
    expect(ASSISTANT_CARD_ENTRY.create).toEqual({ mode: 'kernel-minted-only' });
  });

  it('takes no claim, so it stays on the insertion-ordered fallback scan', () => {
    expect((ASSISTANT_CARD_ENTRY as CardEntry<AssistantCard>).claim).toBeUndefined();
  });

  it('resolves through a really booted registry, ahead of nothing and behind codex', () => {
    const registry = createCardRegistry();
    registerAvailableBuiltinCards(registry);
    expect(registry.resolve({
      id: 'c1', kind: 'codex', payload: { harness_profile: 'assistant' },
    })?.type).toBe('assistant');
    expect(registry.resolve({ id: 'c2', kind: 'codex', payload: {} })?.type).toBe('codex');
    expect(registry.resolve({ id: 'c3', kind: 'codex', payload: { spec_harness: true } })?.type).toBe('spec');
  });
});
