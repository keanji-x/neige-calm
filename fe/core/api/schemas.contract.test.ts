// Unit tests for the WS event zod schemas. Pinned to the discriminated
// union in `schemas.ts`; if the kernel adds a new variant server-side, this
// file is where the parser regression will surface.

import { describe, it, expect, expectTypeOf } from 'vitest';
import type { z } from 'zod';
import {
  wireEventSchema,
  areaSchema,
  trackSchema,
  cardSchema,
  overlaySchema,
} from './schemas.js';
import type {
  Event as GeneratedEvent,
  Area as GeneratedArea,
  Track as GeneratedTrack,
  Card as GeneratedCard,
  Overlay as GeneratedOverlay,
} from './generated/wire.js';
import type { ApiDecodeFailure } from './types.js';
import type { WireEventDecodeResult } from './schemas.js';
import type { DecodeFailure } from '../state/types.js';

describe('wireEventSchema', () => {
  it('parses a valid area.updated event', () => {
    const payload = {
      ev: 'area.updated',
      data: {
        id: 'area_1',
        name: 'Atlas',
        color: '#abc',
        sort: 0,
        kind: 'user',
        created_at: 1000,
        updated_at: 2000,
      },
    };
    const parsed = wireEventSchema.parse(payload);
    expect(parsed.ev).toBe('area.updated');
    if (parsed.ev === 'area.updated') {
      expect(parsed.data.id).toBe('area_1');
      expect(parsed.data.name).toBe('Atlas');
      expect(parsed.data.kind).toBe('user');
    }
  });

  it('defaults area.updated kind to "user" when absent (legacy wire payload)', () => {
    // Issue #175 — `areaKindSchema` carries `.default('user')` so pre-#175
    // wire payloads (event-log replay, legacy fixtures) parse without
    // requiring a fixture migration.
    const payload = {
      ev: 'area.updated',
      data: {
        id: 'area_legacy',
        name: 'Atlas',
        color: '#abc',
        sort: 0,
        created_at: 1000,
        updated_at: 2000,
      },
    };
    const parsed = wireEventSchema.parse(payload);
    if (parsed.ev === 'area.updated') {
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
        track_id: 'track_1',
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

  it('rejects a malformed track (missing required fields)', () => {
    // track.updated requires the full trackSchema; drop `area_id` to force a
    // failure.
    const bad = {
      ev: 'track.updated',
      data: {
        id: 'track_1',
        // area_id missing on purpose
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

  it('preserves agent_message on track.updated payloads', () => {
    const parsed = wireEventSchema.parse({
      ev: 'track.updated',
      data: {
        id: 'track_1',
        area_id: 'area_1',
        title: 'hello',
        sort: 0,
        archived_at: null,
        pinned_at: null,
        lifecycle: 'dispatching',
        cwd: '/repo',
        template_id: null,
        terminal_at: null,
        created_at: 1,
        updated_at: 2,
        agent_message: 'moving to dispatch',
      },
    });
    expect(parsed.ev).toBe('track.updated');
    if (parsed.ev === 'track.updated') {
      expect(parsed.data.agent_message).toBe('moving to dispatch');
      expect(parsed.data.lifecycle).toBe('dispatching');
      // Pre-#891 payload: no `template_input` key — hydrates to the null
      // default rather than failing the parse.
      expect(parsed.data.template_input).toBeNull();
      expect(parsed.data.plugin_scope).toBeNull();
    }
  });

  it('preserves template_input on track.updated payloads (#891)', () => {
    const templateInput = { issue_url: 'https://github.com/o/r/issues/1', issue_number: 1 };
    const parsed = wireEventSchema.parse({
      ev: 'track.updated',
      data: {
        id: 'track_1',
        area_id: 'area_1',
        title: 'hello',
        sort: 0,
        archived_at: null,
        pinned_at: null,
        lifecycle: 'dispatching',
        cwd: '/repo',
        template_id: 'issue-development',
        plugin_scope: 'dev.neige.git-forge',
        template_input: templateInput,
        terminal_at: null,
        created_at: 1,
        updated_at: 2,
      },
    });
    expect(parsed.ev).toBe('track.updated');
    if (parsed.ev === 'track.updated') {
      expect(parsed.data.template_input).toEqual(templateInput);
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
  it('keeps API and state decode failures intentionally shape-equivalent', () => {
    expectTypeOf<ApiDecodeFailure>().toEqualTypeOf<DecodeFailure>();
    expectTypeOf<Extract<WireEventDecodeResult, { status: 'failed' }>['error']>()
      .toEqualTypeOf<ApiDecodeFailure>();
  });
  it('wireEventSchema infers the generated Event union', () => {
    expectTypeOf<z.infer<typeof wireEventSchema>>().toEqualTypeOf<GeneratedEvent>();
  });

  it('entity sub-schemas match their generated counterparts', () => {
    // Per-entity pins make a regression easier to localize than the
    // whole-union check above — a drift in `Card.payload` lights up here
    // before reaching `wireEventSchema`.
    expectTypeOf<z.infer<typeof areaSchema>>().toEqualTypeOf<GeneratedArea>();
    expectTypeOf<z.infer<typeof trackSchema>>().toEqualTypeOf<GeneratedTrack>();
    expectTypeOf<z.infer<typeof cardSchema>>().toEqualTypeOf<GeneratedCard>();
    expectTypeOf<z.infer<typeof overlaySchema>>().toEqualTypeOf<GeneratedOverlay>();
  });
});

describe('entity sub-schema compatibility', () => {
  it('areaSchema fills kind="user" when absent (legacy fixture)', () => {
    const parsed = areaSchema.parse({
      id: 'c1',
      name: 'n',
      color: '#fff',
      sort: 0,
      created_at: 1,
      updated_at: 2,
    });
    expect(parsed.kind).toBe('user');
  });
});

describe('spec harness transcript lifecycle events', () => {
  it('parses harness.transcript.cleared', () => {
    const parsed = wireEventSchema.parse({
      ev: 'harness.transcript.cleared',
      data: {
        runtime_id: 'runtime_2',
        card_id: 'card_spec_1',
        track_id: 'track_1',
        cleared_item_count: 12,
        cleared_params_bytes: 3400,
        card_age_ms_at_clear: 86400000,
      },
    });
    expect(parsed.ev).toBe('harness.transcript.cleared');
    if (parsed.ev === 'harness.transcript.cleared') {
      expect(parsed.data.runtime_id).toBe('runtime_2');
      expect(parsed.data.card_id).toBe('card_spec_1');
      expect(parsed.data.track_id).toBe('track_1');
      expect(parsed.data.cleared_item_count).toBe(12);
      expect(parsed.data.cleared_params_bytes).toBe(3400);
      expect(parsed.data.card_age_ms_at_clear).toBe(86400000);
    }
  });

  it('rejects harness.transcript.cleared missing runtime_id', () => {
    const result = wireEventSchema.safeParse({
      ev: 'harness.transcript.cleared',
      data: {
        card_id: 'card_spec_1',
        track_id: 'track_1',
        cleared_item_count: 12,
        cleared_params_bytes: 3400,
        card_age_ms_at_clear: 86400000,
      },
    });
    expect(result.success).toBe(false);
  });

  // #1252 R1/F1 — the Rust field is `Option<i64>` (so pre-#1252 event rows
  // still replay instead of being dropped), and serde writes `None` as an
  // explicit `null`. So the wire always carries all three keys, and `null`
  // is the "this reset predates the telemetry" value.
  it('parses harness.transcript.cleared with unmeasured (null) telemetry', () => {
    const parsed = wireEventSchema.parse({
      ev: 'harness.transcript.cleared',
      data: {
        runtime_id: 'runtime_2',
        card_id: 'card_spec_1',
        track_id: 'track_1',
        cleared_item_count: null,
        cleared_params_bytes: null,
        card_age_ms_at_clear: null,
      },
    });
    expect(parsed.ev).toBe('harness.transcript.cleared');
    if (parsed.ev === 'harness.transcript.cleared') {
      // null, NOT coerced to 0 — an unmeasured reset must stay
      // distinguishable from one that measured an empty transcript.
      expect(parsed.data.cleared_item_count).toBeNull();
      expect(parsed.data.cleared_params_bytes).toBeNull();
      expect(parsed.data.card_age_ms_at_clear).toBeNull();
    }
  });

  // Still rejected: the keys themselves are not optional. serde emits them
  // on every frame, so an absent key means the producer is not the kernel
  // we think it is.
  it('rejects harness.transcript.cleared missing the reset telemetry keys', () => {
    const result = wireEventSchema.safeParse({
      ev: 'harness.transcript.cleared',
      data: {
        runtime_id: 'runtime_2',
        card_id: 'card_spec_1',
        track_id: 'track_1',
      },
    });
    expect(result.success).toBe(false);
  });

  // The type is still pinned when the field IS present.
  it('rejects harness.transcript.cleared with non-numeric telemetry', () => {
    const result = wireEventSchema.safeParse({
      ev: 'harness.transcript.cleared',
      data: {
        runtime_id: 'runtime_2',
        card_id: 'card_spec_1',
        track_id: 'track_1',
        cleared_item_count: '12',
        cleared_params_bytes: 3400,
        card_age_ms_at_clear: 86400000,
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
        track_id: 'track_1',
        char_count: 9,
      },
    });
    expect(parsed.ev).toBe('harness.user_message.enqueued');
    if (parsed.ev === 'harness.user_message.enqueued') {
      expect(parsed.data.runtime_id).toBe('runtime_2');
      expect(parsed.data.card_id).toBe('card_spec_1');
      expect(parsed.data.track_id).toBe('track_1');
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

  it('parses a valid plan.updated (#644)', () => {
    const parsed = wireEventSchema.parse({
      ev: 'plan.updated',
      data: {
        track_id: 'wv-1',
        changed_keys: ['t1', 't2'],
        agent_message: 'plan revision rationale',
      },
    });
    expect(parsed.ev).toBe('plan.updated');
    if (parsed.ev === 'plan.updated') {
      expect(parsed.data.changed_keys).toEqual(['t1', 't2']);
      expect(parsed.data.agent_message).toBe('plan revision rationale');
    }
  });

  it('rejects plan.updated missing changed_keys', () => {
    const result = wireEventSchema.safeParse({
      ev: 'plan.updated',
      data: { track_id: 'wv-1' },
    });
    expect(result.success).toBe(false);
  });

  it('parses a valid task.gate_result (#644 PR-C), red verdict', () => {
    const parsed = wireEventSchema.parse({
      ev: 'task.gate_result',
      data: {
        task_id: 'wv-1:impl',
        idempotency_key: 'wv-1:impl',
        passed: false,
        failing_step: 'clippy',
        exit_code: 101,
        log_tail: 'error: ...',
        log_path: '/data/gate-logs/wv-1:impl-g1.log',
        attempt: 1,
      },
    });
    expect(parsed.ev).toBe('task.gate_result');
    if (parsed.ev === 'task.gate_result') {
      expect(parsed.data.passed).toBe(false);
      expect(parsed.data.failing_step).toBe('clippy');
      expect(parsed.data.exit_code).toBe(101);
      expect(parsed.data.attempt).toBe(1);
    }
  });

  it('parses a green task.gate_result with the optional fields absent', () => {
    const parsed = wireEventSchema.parse({
      ev: 'task.gate_result',
      data: {
        task_id: 'wv-1:impl',
        idempotency_key: 'wv-1:impl',
        passed: true,
        exit_code: 0,
        log_tail: '',
        log_path: '/data/gate-logs/wv-1:impl-g1.log',
        attempt: 1,
      },
    });
    expect(parsed.ev).toBe('task.gate_result');
    if (parsed.ev === 'task.gate_result') {
      expect(parsed.data.passed).toBe(true);
      expect(parsed.data.failing_step).toBeUndefined();
    }
  });

  it('rejects task.gate_result missing passed', () => {
    const result = wireEventSchema.safeParse({
      ev: 'task.gate_result',
      data: {
        task_id: 'wv-1:impl',
        idempotency_key: 'wv-1:impl',
        log_tail: '',
        log_path: '/p',
        attempt: 1,
      },
    });
    expect(result.success).toBe(false);
  });

  it('parses a valid plugin.tool.registered (#760 slice 2)', () => {
    const parsed = wireEventSchema.parse({
      ev: 'plugin.tool.registered',
      data: {
        plugin_id: 'dev.echo',
        tool_name: 'do.thing',
      },
    });
    expect(parsed.ev).toBe('plugin.tool.registered');
    if (parsed.ev === 'plugin.tool.registered') {
      expect(parsed.data.plugin_id).toBe('dev.echo');
      expect(parsed.data.tool_name).toBe('do.thing');
    }
  });

  it('rejects plugin.tool.registered missing tool_name', () => {
    const result = wireEventSchema.safeParse({
      ev: 'plugin.tool.registered',
      data: {
        plugin_id: 'dev.echo',
      },
    });
    expect(result.success).toBe(false);
  });

  it('parses a valid forge.issue.read (#760 slice 4b)', () => {
    const parsed = wireEventSchema.parse({
      ev: 'forge.issue.read',
      data: {
        track_id: 'track-01',
        issue_number: 813,
        artifact_path: '/tmp/neige/issue-body.md',
      },
    });
    expect(parsed.ev).toBe('forge.issue.read');
    if (parsed.ev === 'forge.issue.read') {
      expect(parsed.data.track_id).toBe('track-01');
      expect(parsed.data.issue_number).toBe(813);
      expect(parsed.data.artifact_path).toBe('/tmp/neige/issue-body.md');
    }
  });
});

// ---- PR2 of #247: track.report_edited ----------------------------------
//
// Structured edit-log companion to `card.updated`. Card-scoped. PR4
// (web UI) and PR5 (spec agent) both subscribe to it; the parser must
// accept the three `author` discriminator values + reject missing
// required fields without falling back to a permissive shape.
describe('PR2 of #247: track.report_edited', () => {
  it('parses a valid track.report_edited with author=spec', () => {
    const parsed = wireEventSchema.parse({
      ev: 'track.report_edited',
      data: {
        track_id: 'w-1',
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
    expect(parsed.ev).toBe('track.report_edited');
    if (parsed.ev === 'track.report_edited') {
      expect(parsed.data.author).toBe('spec');
      expect(parsed.data.track_id).toBe('w-1');
      expect(parsed.data.card_id).toBe('card-1');
      expect(parsed.data.body_after).toBe('new body');
      expect(parsed.data.agent_message).toBe('report rationale');
    }
  });

  it('accepts every author discriminator (spec | user | kernel | plugin)', () => {
    for (const author of [
      'spec',
      'user',
      'assistant',
      'kernel',
      'plugin',
    ] as const) {
      const parsed = wireEventSchema.parse({
        ev: 'track.report_edited',
        data: {
          track_id: 'w',
          card_id: 'c',
          author,
          edit_id: 'edit-1',
          summary_before: '',
          summary_after: '',
          body_before: '',
          body_after: '',
        },
      });
      if (parsed.ev === 'track.report_edited') {
        expect(parsed.data.author).toBe(author);
      }
    }
  });

  it('parses the #955 plugin author arm with author_plugin_id', () => {
    const parsed = wireEventSchema.parse({
      ev: 'track.report_edited',
      data: {
        track_id: 'w',
        card_id: 'c',
        author: 'plugin',
        author_plugin_id: 'dev.neige.invest',
        edit_id: 'edit-1',
        summary_before: '',
        summary_after: '',
        body_before: '',
        body_after: '',
      },
    });
    if (parsed.ev === 'track.report_edited') {
      expect(parsed.data.author).toBe('plugin');
      expect(parsed.data.author_plugin_id).toBe('dev.neige.invest');
    }
  });

  it('rejects track.report_edited with an unknown author', () => {
    const result = wireEventSchema.safeParse({
      ev: 'track.report_edited',
      data: {
        track_id: 'w',
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

  it('rejects track.report_edited missing edit_id', () => {
    const result = wireEventSchema.safeParse({
      ev: 'track.report_edited',
      data: {
        track_id: 'w',
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

  it('rejects track.report_edited missing body_after', () => {
    const result = wireEventSchema.safeParse({
      ev: 'track.report_edited',
      data: {
        track_id: 'w',
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
  it('areaSchema round-trips a minimal area', () => {
    const c = {
      id: 'c1',
      name: 'n',
      color: '#fff',
      sort: 0,
      kind: 'user' as const,
      created_at: 1,
      updated_at: 2,
    };
    expect(areaSchema.parse(c)).toEqual(c);
  });

  it('trackSchema accepts archived_at: null', () => {
    const w = {
      id: 'w1',
      area_id: 'c1',
      title: 't',
      sort: 0,
      archived_at: null,
      created_at: 1,
      updated_at: 2,
    };
    expect(trackSchema.parse(w).archived_at).toBeNull();
  });

  // ---------------- Issue #145 — Track lifecycle ----------------

  it('trackSchema defaults `lifecycle` to "draft" when the field is missing', () => {
    // Pre-#145 wire payloads (event-log replay fixtures from older
    // kernels, recorded sessions) carry no `lifecycle`. The schema
    // default + the Rust struct's `#[serde(default)]` keep them
    // parseable; the parsed value is always `draft` for the back-
    // compat path.
    const w = {
      id: 'w1',
      area_id: 'c1',
      title: 't',
      sort: 0,
      archived_at: null,
      created_at: 1,
      updated_at: 2,
    };
    expect(trackSchema.parse(w).lifecycle).toBe('draft');
  });

  it('trackSchema hydrates + preserves `workspace` (#1147 S1)', () => {
    // Two halves, and the second is the one that has burned this repo before:
    // an undeclared field is *stripped* by zod, so a server that sends
    // `workspace` and a client that never declared it look identical to a
    // pre-#1147 replay payload. The "missing key" case pins the default; the
    // "present key" case pins that the field actually survives parsing.
    const base = {
      id: 'w1',
      area_id: 'c1',
      title: 't',
      sort: 0,
      archived_at: null,
      created_at: 1,
      updated_at: 2,
    };
    expect(trackSchema.parse(base).workspace).toEqual({
      kind: 'attached',
      path: '',
      frozen_at: null,
    });
    const live = trackSchema.parse({
      ...base,
      cwd: '/srv/neige-workspaces/c1/w1',
      workspace: {
        kind: 'managed',
        path: '/srv/neige-workspaces/c1/w1',
        frozen_at: 4242,
      },
    });
    expect(live.workspace).toEqual({
      kind: 'managed',
      path: '/srv/neige-workspaces/c1/w1',
      frozen_at: 4242,
    });
    // NB: there is deliberately no `live.workspace.path === live.cwd`
    // assertion here. Both values come from this fixture's own literal and
    // zod has no cross-field constraint, so it would be true no matter what
    // the schema said. The projection invariant is a *server* property and is
    // asserted where it can fail — `track_workspace_migration_tests` in
    // calm-truth, against a real row.
  });

  it('trackSchema rejects a present-but-incomplete `workspace` (#1147 S1)', () => {
    // Absent key ⇒ default (old payloads keep working). Present key ⇒ every
    // field required, because a partial object means the server is wrong and
    // silently defaulting it would hide a regression or a half-rolled deploy.
    // Mirrors serde: none of the three fields has `#[serde(default)]`.
    const base = {
      id: 'w1',
      area_id: 'c1',
      title: 't',
      sort: 0,
      archived_at: null,
      created_at: 1,
      updated_at: 2,
    };
    for (const bad of [
      {},
      { kind: 'managed' },
      { path: '/p', frozen_at: null },
      { kind: 'managed', path: '/p' },
      { kind: 'bogus', path: '/p', frozen_at: null },
    ]) {
      expect(trackSchema.safeParse({ ...base, workspace: bad }).success).toBe(
        false,
      );
    }
    expect(
      trackSchema.safeParse({
        ...base,
        workspace: { kind: 'managed', path: '/p', frozen_at: null },
      }).success,
    ).toBe(true);
  });

  it('trackSchema round-trips every lifecycle name', () => {
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
        area_id: 'c1',
        title: 't',
        sort: 0,
        archived_at: null,
        lifecycle: lc,
        created_at: 1,
        updated_at: 2,
      };
      expect(trackSchema.parse(w).lifecycle).toBe(lc);
    }
  });

  it('wireEventSchema parses track.lifecycle_changed envelopes', () => {
    const env = {
      ev: 'track.lifecycle_changed',
      data: {
        id: 'w1',
        area_id: 'c1',
        from: 'draft',
        to: 'planning',
        agent_message: 'planning rationale',
      },
    };
    const parsed = wireEventSchema.parse(env);
    expect(parsed.ev).toBe('track.lifecycle_changed');
    if (parsed.ev === 'track.lifecycle_changed') {
      expect(parsed.data.from).toBe('draft');
      expect(parsed.data.to).toBe('planning');
      expect(parsed.data.agent_message).toBe('planning rationale');
    }
  });
});

// ---- #955 §5 PR-a: proposal-channel events ------------------------------
describe('#955: proposal events', () => {
  it('parses proposal.submitted with every op shape', () => {
    const parsed = wireEventSchema.parse({
      ev: 'proposal.submitted',
      data: {
        track_id: 'w-1',
        proposal_id: 'pp-1',
        plugin_id: 'dev.neige.invest',
        subject_kind: 'report',
        base_doc_heads: 'ah1:deadbeef',
        ops: [
          {
            op: 'upsert_block',
            block_id: 'b_0001',
            kind: 'prose',
            payload: { markdown: 'revised\n' },
            if_rev: 3,
          },
          {
            op: 'upsert_block',
            temp_id: 't1',
            kind: 'prose',
            payload: { markdown: '# New\n' },
            anchor: 'at_end',
          },
          {
            op: 'move_block',
            block_id: 'b_0002',
            if_rev: 1,
            anchor: { after_block_id: 'temp:t1' },
          },
          { op: 'delete_block', block_id: 'b_0003', if_rev: 2 },
        ],
        note: 'why',
        idem_key: 'idem-1',
      },
    });
    expect(parsed.ev).toBe('proposal.submitted');
    if (parsed.ev === 'proposal.submitted') {
      expect(parsed.data.ops).toHaveLength(4);
      expect(parsed.data.plugin_id).toBe('dev.neige.invest');
    }
  });

  it('parses proposal.resolved for every decision', () => {
    for (const decision of ['accepted', 'rejected', 'stale', 'withdrawn'] as const) {
      const parsed = wireEventSchema.parse({
        ev: 'proposal.resolved',
        data: {
          track_id: 'w-1',
          proposal_id: 'pp-1',
          plugin_id: 'dev.neige.invest',
          decision,
        },
      });
      if (parsed.ev === 'proposal.resolved') {
        expect(parsed.data.decision).toBe(decision);
      }
    }
  });

  it('rejects an unknown decision and a malformed op', () => {
    expect(
      wireEventSchema.safeParse({
        ev: 'proposal.resolved',
        data: {
          track_id: 'w-1',
          proposal_id: 'pp-1',
          plugin_id: 'p',
          decision: 'merged',
        },
      }).success,
    ).toBe(false);
    expect(
      wireEventSchema.safeParse({
        ev: 'proposal.submitted',
        data: {
          track_id: 'w-1',
          proposal_id: 'pp-1',
          plugin_id: 'p',
          subject_kind: 'report',
          base_doc_heads: 'ah1:x',
          ops: [{ op: 'delete_block', block_id: 'b_1' }],
          note: '',
          idem_key: 'k',
        },
      }).success,
    ).toBe(false);
  });
});

// #1209 PR-2 test #14 (design §3.4) — historical `track.updated` rows and REST
// replays still spell the template fields with the pre-rename keys. The Rust
// side keeps a deserialize-only `#[serde(alias)]`; this reader keeps the
// matching one-way normalize. Without it the schema's `.default(null)` would
// hydrate every historical row as `template_id: null` — silently.
//
// One of THREE independent copies of this normalize (the other two live in
// `web/src/api/schemas.ts` and `web/src/track-fs-viewers/schemas.ts`). They are deliberately not
// factored into a shared helper: "only the third reader was missed" has to be a
// red test, not a green one.
describe('#1209 pre-rename template keys on the track shape', () => {
  const legacyTrack = {
    id: 'w1',
    area_id: 'c1',
    title: 't',
    sort: 0,
    archived_at: null,
    workflow_id: 'small-change',
    workflow_input: { issue: 1209 },
    created_at: 1,
    updated_at: 2,
  };

  it('recovers `template_id` from a legacy `workflow_id` key', () => {
    expect(trackSchema.parse(legacyTrack).template_id).toBe('small-change');
  });

  it('recovers `template_input` from a legacy `workflow_input` key', () => {
    expect(trackSchema.parse(legacyTrack).template_input).toEqual({ issue: 1209 });
  });

  it('does not let a legacy key overwrite a present new key', () => {
    const parsed = trackSchema.parse({
      ...legacyTrack,
      template_id: 'investigation',
      template_input: { issue: 1 },
    });
    expect(parsed.template_id).toBe('investigation');
    expect(parsed.template_input).toEqual({ issue: 1 });
  });

  it('normalizes inside a `track.updated` event payload too', () => {
    const parsed = wireEventSchema.parse({
      ev: 'track.updated',
      data: { ...legacyTrack, agent_message: 'hi' },
    });
    if (parsed.ev !== 'track.updated') throw new Error('wrong variant');
    expect(parsed.data.template_id).toBe('small-change');
    expect(parsed.data.template_input).toEqual({ issue: 1209 });
  });

  it('still hydrates a payload that carries neither spelling', () => {
    const bare = { ...legacyTrack } as Record<string, unknown>;
    delete bare.workflow_id;
    delete bare.workflow_input;
    const parsed = trackSchema.parse(bare);
    expect(parsed.template_id).toBeNull();
    expect(parsed.template_input).toBeNull();
  });
});
