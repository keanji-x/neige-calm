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
        name: 'Scratch',
        color: '#abc',
        sort: 0,
        created_at: 1000,
        updated_at: 2000,
      },
    };
    const parsed = wireEventSchema.parse(payload);
    expect(parsed.ev).toBe('cove.updated');
    if (parsed.ev === 'cove.updated') {
      expect(parsed.data.id).toBe('cove_1');
      expect(parsed.data.name).toBe('Scratch');
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

describe('entity sub-schemas', () => {
  it('coveSchema round-trips a minimal cove', () => {
    const c = {
      id: 'c1',
      name: 'n',
      color: '#fff',
      sort: 0,
      created_at: 1,
      updated_at: 2,
    };
    expect(coveSchema.parse(c)).toEqual(c);
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
