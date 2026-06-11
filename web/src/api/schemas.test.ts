// Unit tests for the WS event zod schemas. Pinned to the discriminated
// union in `schemas.ts`; if the kernel adds a new variant server-side, this
// file is where the parser regression will surface.

import { describe, it, expect, expectTypeOf } from 'vitest';
import type { z } from 'zod';
import {
  wireEventSchema,
  coveSchema,
  waveSchema,
  cardSchema,
  overlaySchema,
} from './schemas';
import type {
  Event as GeneratedEvent,
  Cove as GeneratedCove,
  Wave as GeneratedWave,
  Card as GeneratedCard,
  Overlay as GeneratedOverlay,
} from './generated-events';

describe('wireEventSchema', () => {
  it('parses a valid cove.updated event', () => {
    const payload = {
      ev: 'cove.updated',
      data: {
        id: 'cove_1',
        name: 'Atlas',
        color: '#abc',
        sort: 0,
        kind: 'user',
        created_at: 1000,
        updated_at: 2000,
      },
    };
    const parsed = wireEventSchema.parse(payload);
    expect(parsed.ev).toBe('cove.updated');
    if (parsed.ev === 'cove.updated') {
      expect(parsed.data.id).toBe('cove_1');
      expect(parsed.data.name).toBe('Atlas');
      expect(parsed.data.kind).toBe('user');
    }
  });

  it('defaults cove.updated kind to "user" when absent (legacy wire payload)', () => {
    // Issue #175 — `coveKindSchema` carries `.default('user')` so pre-#175
    // wire payloads (event-log replay, legacy fixtures) parse without
    // requiring a fixture migration.
    const payload = {
      ev: 'cove.updated',
      data: {
        id: 'cove_legacy',
        name: 'Atlas',
        color: '#abc',
        sort: 0,
        created_at: 1000,
        updated_at: 2000,
      },
    };
    const parsed = wireEventSchema.parse(payload);
    if (parsed.ev === 'cove.updated') {
      expect(parsed.data.kind).toBe('user');
    }
  });

  it('parses card.added with an arbitrary unknown payload blob', () => {
    // `payload` on a kernel card is `serde_json::Value`; the schema accepts
    // anything. Throw a deeply-nested object at it to make sure z.unknown()
    // really is permissive.
    const cardPayload = { terminal_id: 't_42', nested: { foo: [1, 2, 3] } };
    const event = {
      ev: 'card.added',
      data: {
        id: 'card_1',
        wave_id: 'wave_1',
        kind: 'terminal',
        sort: 5,
        payload: cardPayload,
        created_at: 1000,
        updated_at: 2000,
      },
    };
    const parsed = wireEventSchema.parse(event);
    expect(parsed.ev).toBe('card.added');
    if (parsed.ev === 'card.added') {
      expect(parsed.data.kind).toBe('terminal');
      expect(parsed.data.payload).toEqual(cardPayload);
    }
  });

  it('rejects an unknown ev string via safeParse', () => {
    const result = wireEventSchema.safeParse({
      ev: 'totally.made.up',
      data: { id: 'x' },
    });
    expect(result.success).toBe(false);
    // The discriminator should surface in the issues — the exact issue code
    // varies by zod version, but we always see at least one issue.
    if (!result.success) {
      expect(result.error.issues.length).toBeGreaterThan(0);
    }
  });

  it('rejects a malformed wave (missing required fields)', () => {
    // wave.updated requires the full waveSchema; drop `cove_id` to force a
    // failure.
    const bad = {
      ev: 'wave.updated',
      data: {
        id: 'wave_1',
        // cove_id missing on purpose
        title: 'hello',
        sort: 0,
        archived_at: null,
        created_at: 1,
        updated_at: 2,
      },
    };
    const result = wireEventSchema.safeParse(bad);
    expect(result.success).toBe(false);
  });

  it('preserves agent_message on wave.updated payloads', () => {
    const parsed = wireEventSchema.parse({
      ev: 'wave.updated',
      data: {
        id: 'wave_1',
        cove_id: 'cove_1',
        title: 'hello',
        sort: 0,
        archived_at: null,
        pinned_at: null,
        lifecycle: 'dispatching',
        cwd: '/repo',
        terminal_at: null,
        created_at: 1,
        updated_at: 2,
        agent_message: 'moving to dispatch',
      },
    });
    expect(parsed.ev).toBe('wave.updated');
    if (parsed.ev === 'wave.updated') {
      expect(parsed.data.agent_message).toBe('moving to dispatch');
      expect(parsed.data.lifecycle).toBe('dispatching');
    }
  });

  it('parses a valid claude.hook event', () => {
    const payload = { hook_event_name: 'PreToolUse', tool_name: 'Bash' };
    const parsed = wireEventSchema.parse({
      ev: 'claude.hook',
      data: {
        card_id: 'card_claude_1',
        kind: 'hook.claude.pre_tool_use',
        hook_idempotency_key: 'test-key',
        payload,
      },
    });
    expect(parsed.ev).toBe('claude.hook');
    if (parsed.ev === 'claude.hook') {
      expect(parsed.data.card_id).toBe('card_claude_1');
      expect(parsed.data.kind).toBe('hook.claude.pre_tool_use');
      expect(parsed.data.hook_idempotency_key).toBe('test-key');
      expect(parsed.data.payload).toEqual(payload);
    }
  });
});

// ---------------- ts-rs ↔ zod conformance (D7 / issue #5) ----------------
//
// These assertions pin the runtime zod schemas to the TS types emitted by
// `ts-rs` from the Rust `Event` enum. The generator is the single source of
// truth; the zod schemas in `schemas.ts` only exist for runtime validation
// at the WS boundary. If a Rust-side change drifts ahead of zod (or vice
// versa), the project's `tsc -b` step (run during `npm run build` and on
// each `npm run test` via vitest's type-check inference) fails right here.
//
// We use `expectTypeOf(...).toEqualTypeOf<...>()` for bidirectional
// assignability. The whole-`Event`-union check is the bigger guarantee;
// the per-entity checks make a regression easier to localize.
describe('zod ↔ ts-rs conformance', () => {
  it('wireEventSchema infers the generated Event union', () => {
    expectTypeOf<z.infer<typeof wireEventSchema>>().toEqualTypeOf<GeneratedEvent>();
  });

  it('entity sub-schemas match their generated counterparts', () => {
    // Per-entity pins make a regression easier to localize than the
    // whole-union check above — a drift in `Card.payload` lights up here
    // before reaching `wireEventSchema`.
    expectTypeOf<z.infer<typeof coveSchema>>().toEqualTypeOf<GeneratedCove>();
    expectTypeOf<z.infer<typeof waveSchema>>().toEqualTypeOf<GeneratedWave>();
    expectTypeOf<z.infer<typeof cardSchema>>().toEqualTypeOf<GeneratedCard>();
    expectTypeOf<z.infer<typeof overlaySchema>>().toEqualTypeOf<GeneratedOverlay>();
  });
});

describe('spec harness transcript lifecycle events', () => {
  it('parses harness.transcript.cleared', () => {
    const parsed = wireEventSchema.parse({
      ev: 'harness.transcript.cleared',
      data: {
        runtime_id: 'runtime_2',
        card_id: 'card_spec_1',
        wave_id: 'wave_1',
      },
    });
    expect(parsed.ev).toBe('harness.transcript.cleared');
    if (parsed.ev === 'harness.transcript.cleared') {
      expect(parsed.data.runtime_id).toBe('runtime_2');
      expect(parsed.data.card_id).toBe('card_spec_1');
      expect(parsed.data.wave_id).toBe('wave_1');
    }
  });

  it('rejects harness.transcript.cleared missing runtime_id', () => {
    const result = wireEventSchema.safeParse({
      ev: 'harness.transcript.cleared',
      data: {
        card_id: 'card_spec_1',
        wave_id: 'wave_1',
      },
    });
    expect(result.success).toBe(false);
  });

  it('parses harness.user_message.enqueued without body text', () => {
    const parsed = wireEventSchema.parse({
      ev: 'harness.user_message.enqueued',
      data: {
        runtime_id: 'runtime_2',
        card_id: 'card_spec_1',
        wave_id: 'wave_1',
        char_count: 9,
      },
    });
    expect(parsed.ev).toBe('harness.user_message.enqueued');
    if (parsed.ev === 'harness.user_message.enqueued') {
      expect(parsed.data.runtime_id).toBe('runtime_2');
      expect(parsed.data.card_id).toBe('card_spec_1');
      expect(parsed.data.wave_id).toBe('wave_1');
      expect(parsed.data.char_count).toBe(9);
      expect('text' in parsed.data).toBe(false);
    }
  });
});

// ---- PR4 of #136: dispatcher + task-lifecycle variants ----------------
//
// Schema-only PR. These tests pin the wire shape the parser accepts/rejects
// for each of the four new variants. Two per variant: a happy-path parse,
// and a `safeParse` confirming a missing required field fails. PR5's
// Dispatcher will emit these payloads — these tests are the contract
// they're emitting against.
describe('PR4 of #136: dispatcher + task-lifecycle variants', () => {
  it('parses a valid codex.worker_requested', () => {
    const parsed = wireEventSchema.parse({
      ev: 'codex.worker_requested',
      data: {
        idempotency_key: 'idem-1',
        goal: 'refactor X',
        context: { cwd: '/tmp', hints: [1, 2] },
        acceptance_criteria: 'tests pass',
        agent_message: 'dispatch codex rationale',
      },
    });
    expect(parsed.ev).toBe('codex.worker_requested');
    if (parsed.ev === 'codex.worker_requested') {
      expect(parsed.data.idempotency_key).toBe('idem-1');
      expect(parsed.data.goal).toBe('refactor X');
      expect(parsed.data.agent_message).toBe('dispatch codex rationale');
    }
  });

  it('rejects codex.worker_requested missing idempotency_key', () => {
    const result = wireEventSchema.safeParse({
      ev: 'codex.worker_requested',
      data: { goal: 'g', context: {} },
    });
    expect(result.success).toBe(false);
  });

  it('parses a valid terminal.worker_requested (cwd present)', () => {
    const parsed = wireEventSchema.parse({
      ev: 'terminal.worker_requested',
      data: {
        idempotency_key: 'idem-2',
        cmd: 'cargo test',
        cwd: '/repo',
        agent_message: 'dispatch terminal rationale',
      },
    });
    expect(parsed.ev).toBe('terminal.worker_requested');
    if (parsed.ev === 'terminal.worker_requested') {
      expect(parsed.data.cmd).toBe('cargo test');
      expect(parsed.data.cwd).toBe('/repo');
      expect(parsed.data.agent_message).toBe('dispatch terminal rationale');
    }
  });

  it('rejects terminal.worker_requested missing cmd', () => {
    const result = wireEventSchema.safeParse({
      ev: 'terminal.worker_requested',
      data: { idempotency_key: 'idem-2' },
    });
    expect(result.success).toBe(false);
  });

  it('parses a valid task.completed (artifacts as bare strings)', () => {
    // `ArtifactRef` is `#[serde(transparent)]` around `String` on the
    // server, so each artifacts[] element is a bare string on the wire.
    const parsed = wireEventSchema.parse({
      ev: 'task.completed',
      data: {
        idempotency_key: 'idem-3',
        result: { summary: 'ok', lines: 42 },
        artifacts: ['a-1', 'a-2'],
        agent_message: 'worker completed rationale',
      },
    });
    expect(parsed.ev).toBe('task.completed');
    if (parsed.ev === 'task.completed') {
      expect(parsed.data.artifacts).toEqual(['a-1', 'a-2']);
      expect(parsed.data.agent_message).toBe('worker completed rationale');
    }
  });

  it('rejects task.completed missing artifacts array', () => {
    const result = wireEventSchema.safeParse({
      ev: 'task.completed',
      data: { idempotency_key: 'idem-3', result: {} },
    });
    expect(result.success).toBe(false);
  });

  it('parses a valid task.failed', () => {
    const parsed = wireEventSchema.parse({
      ev: 'task.failed',
      data: {
        idempotency_key: 'idem-4',
        reason: 'process exited with code 137',
        agent_message: 'worker failed rationale',
      },
    });
    expect(parsed.ev).toBe('task.failed');
    if (parsed.ev === 'task.failed') {
      expect(parsed.data.reason).toBe('process exited with code 137');
      expect(parsed.data.agent_message).toBe('worker failed rationale');
    }
  });

  it('rejects task.failed missing reason', () => {
    const result = wireEventSchema.safeParse({
      ev: 'task.failed',
      data: { idempotency_key: 'idem-4' },
    });
    expect(result.success).toBe(false);
  });
});

// ---- PR2 of #247: wave.report_edited ----------------------------------
//
// Structured edit-log companion to `card.updated`. Card-scoped. PR4
// (web UI) and PR5 (spec agent) both subscribe to it; the parser must
// accept the three `author` discriminator values + reject missing
// required fields without falling back to a permissive shape.
describe('PR2 of #247: wave.report_edited', () => {
  it('parses a valid wave.report_edited with author=spec', () => {
    const parsed = wireEventSchema.parse({
      ev: 'wave.report_edited',
      data: {
        wave_id: 'w-1',
        card_id: 'card-1',
        author: 'spec',
        edit_id: '00000000-0000-4000-8000-000000000000',
        summary_before: 'old summary',
        summary_after: 'new summary',
        body_before: 'old body',
        body_after: 'new body',
        agent_message: 'report rationale',
      },
    });
    expect(parsed.ev).toBe('wave.report_edited');
    if (parsed.ev === 'wave.report_edited') {
      expect(parsed.data.author).toBe('spec');
      expect(parsed.data.wave_id).toBe('w-1');
      expect(parsed.data.card_id).toBe('card-1');
      expect(parsed.data.body_after).toBe('new body');
      expect(parsed.data.agent_message).toBe('report rationale');
    }
  });

  it('accepts every author discriminator (spec | user | kernel)', () => {
    for (const author of ['spec', 'user', 'kernel'] as const) {
      const parsed = wireEventSchema.parse({
        ev: 'wave.report_edited',
        data: {
          wave_id: 'w',
          card_id: 'c',
          author,
          edit_id: 'edit-1',
          summary_before: '',
          summary_after: '',
          body_before: '',
          body_after: '',
        },
      });
      if (parsed.ev === 'wave.report_edited') {
        expect(parsed.data.author).toBe(author);
      }
    }
  });

  it('rejects wave.report_edited with an unknown author', () => {
    const result = wireEventSchema.safeParse({
      ev: 'wave.report_edited',
      data: {
        wave_id: 'w',
        card_id: 'c',
        author: 'bot',
        edit_id: 'edit-1',
        summary_before: '',
        summary_after: '',
        body_before: '',
        body_after: '',
      },
    });
    expect(result.success).toBe(false);
  });

  it('rejects wave.report_edited missing edit_id', () => {
    const result = wireEventSchema.safeParse({
      ev: 'wave.report_edited',
      data: {
        wave_id: 'w',
        card_id: 'c',
        author: 'spec',
        summary_before: '',
        summary_after: '',
        body_before: '',
        body_after: '',
      },
    });
    expect(result.success).toBe(false);
  });

  it('rejects wave.report_edited missing body_after', () => {
    const result = wireEventSchema.safeParse({
      ev: 'wave.report_edited',
      data: {
        wave_id: 'w',
        card_id: 'c',
        author: 'spec',
        edit_id: 'edit-1',
        summary_before: '',
        summary_after: '',
        body_before: '',
      },
    });
    expect(result.success).toBe(false);
  });
});

describe('entity sub-schemas', () => {
  it('coveSchema round-trips a minimal cove', () => {
    const c = {
      id: 'c1',
      name: 'n',
      color: '#fff',
      sort: 0,
      kind: 'user' as const,
      created_at: 1,
      updated_at: 2,
    };
    expect(coveSchema.parse(c)).toEqual(c);
  });

  it('coveSchema fills kind="user" when absent (legacy fixture)', () => {
    // Issue #175 — same default story as the event-schema test above:
    // pre-#175 wire payloads must round-trip without forcing a fixture
    // migration on every recorded session.
    const c = {
      id: 'c1',
      name: 'n',
      color: '#fff',
      sort: 0,
      created_at: 1,
      updated_at: 2,
    };
    const parsed = coveSchema.parse(c);
    expect(parsed.kind).toBe('user');
  });

  it('waveSchema accepts archived_at: null', () => {
    const w = {
      id: 'w1',
      cove_id: 'c1',
      title: 't',
      sort: 0,
      archived_at: null,
      created_at: 1,
      updated_at: 2,
    };
    expect(waveSchema.parse(w).archived_at).toBeNull();
  });

  // ---------------- Issue #145 — Wave lifecycle ----------------

  it('waveSchema defaults `lifecycle` to "draft" when the field is missing', () => {
    // Pre-#145 wire payloads (event-log replay fixtures from older
    // kernels, recorded sessions) carry no `lifecycle`. The schema
    // default + the Rust struct's `#[serde(default)]` keep them
    // parseable; the parsed value is always `draft` for the back-
    // compat path.
    const w = {
      id: 'w1',
      cove_id: 'c1',
      title: 't',
      sort: 0,
      archived_at: null,
      created_at: 1,
      updated_at: 2,
    };
    expect(waveSchema.parse(w).lifecycle).toBe('draft');
  });

  it('waveSchema round-trips every lifecycle name', () => {
    const all = [
      'draft',
      'planning',
      'dispatching',
      'working',
      'blocked',
      'reviewing',
      'done',
      'canceled',
      'failed',
    ] as const;
    for (const lc of all) {
      const w = {
        id: 'w1',
        cove_id: 'c1',
        title: 't',
        sort: 0,
        archived_at: null,
        lifecycle: lc,
        created_at: 1,
        updated_at: 2,
      };
      expect(waveSchema.parse(w).lifecycle).toBe(lc);
    }
  });

  it('wireEventSchema parses wave.lifecycle_changed envelopes', () => {
    const env = {
      ev: 'wave.lifecycle_changed',
      data: {
        id: 'w1',
        cove_id: 'c1',
        from: 'draft',
        to: 'planning',
        agent_message: 'planning rationale',
      },
    };
    const parsed = wireEventSchema.parse(env);
    expect(parsed.ev).toBe('wave.lifecycle_changed');
    if (parsed.ev === 'wave.lifecycle_changed') {
      expect(parsed.data.from).toBe('draft');
      expect(parsed.data.to).toBe('planning');
      expect(parsed.data.agent_message).toBe('planning rationale');
    }
  });
});
