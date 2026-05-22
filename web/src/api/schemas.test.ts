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

// ---- PR4 of #136: dispatcher + task-lifecycle variants ----------------
//
// Schema-only PR. These tests pin the wire shape the parser accepts/rejects
// for each of the four new variants. Two per variant: a happy-path parse,
// and a `safeParse` confirming a missing required field fails. PR5's
// Dispatcher and PR8's wait_for_events will emit these payloads — these
// tests are the contract they're emitting against.
describe('PR4 of #136: dispatcher + task-lifecycle variants', () => {
  it('parses a valid codex.job_requested', () => {
    const parsed = wireEventSchema.parse({
      ev: 'codex.job_requested',
      data: {
        idempotency_key: 'idem-1',
        goal: 'refactor X',
        context: { cwd: '/tmp', hints: [1, 2] },
        acceptance_criteria: 'tests pass',
      },
    });
    expect(parsed.ev).toBe('codex.job_requested');
    if (parsed.ev === 'codex.job_requested') {
      expect(parsed.data.idempotency_key).toBe('idem-1');
      expect(parsed.data.goal).toBe('refactor X');
    }
  });

  it('rejects codex.job_requested missing idempotency_key', () => {
    const result = wireEventSchema.safeParse({
      ev: 'codex.job_requested',
      data: { goal: 'g', context: {} },
    });
    expect(result.success).toBe(false);
  });

  it('parses a valid terminal.job_requested (cwd present)', () => {
    const parsed = wireEventSchema.parse({
      ev: 'terminal.job_requested',
      data: { idempotency_key: 'idem-2', cmd: 'cargo test', cwd: '/repo' },
    });
    expect(parsed.ev).toBe('terminal.job_requested');
    if (parsed.ev === 'terminal.job_requested') {
      expect(parsed.data.cmd).toBe('cargo test');
      expect(parsed.data.cwd).toBe('/repo');
    }
  });

  it('rejects terminal.job_requested missing cmd', () => {
    const result = wireEventSchema.safeParse({
      ev: 'terminal.job_requested',
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
      },
    });
    expect(parsed.ev).toBe('task.completed');
    if (parsed.ev === 'task.completed') {
      expect(parsed.data.artifacts).toEqual(['a-1', 'a-2']);
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
      },
    });
    expect(parsed.ev).toBe('task.failed');
    if (parsed.ev === 'task.failed') {
      expect(parsed.data.reason).toBe('process exited with code 137');
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
});
