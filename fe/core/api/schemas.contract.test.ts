// Unit tests for the WS event zod schemas.

import { describe, it, expect, expectTypeOf } from 'vitest';
import type { z } from 'zod';
import {
  wireEventSchema,
  areaSchema,
  trackSchema,
  cardSchema,
  overlaySchema,
} from './schemas.js';
import { areaFolderWireSchema, folderConflictSchema } from '../domain/area.js';
import type {
  EventWire as GeneratedEvent,
  Area as GeneratedArea,
  TrackWire as GeneratedTrack,
  Card as GeneratedCard,
  Overlay as GeneratedOverlay,
  AreaFolder as GeneratedAreaFolder,
  FolderConflict as GeneratedFolderConflict,
} from './generated/wire.js';
import type { ApiDecodeFailure } from './types.js';
import type { WireEventDecodeResult } from './schemas.js';
import type { DecodeFailure } from '../state/types.js';

describe('wireEventSchema', () => {
  it('defaults area.updated kind to "user" when absent (legacy wire payload)', () => {
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
      expect(parsed.data).toMatchObject({
        kind: 'user', default_template_id: null, default_cwd: null,
      });
    }
  });

  it('rejects an unknown ev string via safeParse', () => {
    const result = wireEventSchema.safeParse({
      ev: 'totally.made.up',
      data: { id: 'x' },
    });
    expect(result.success).toBe(false);
    // The exact issue code varies by zod version.
    if (!result.success) {
      expect(result.error.issues.length).toBeGreaterThan(0);
    }
  });

  it('defaults absent template metadata on track.updated payloads', () => {
    const parsed = wireEventSchema.parse({
      ev: 'track.updated',
      data: { workspace: { isolation: 'none' as const, kind: 'attached' as const, path: '/repo', frozen_at: null },
        id: 'track_1',
        area_id: 'area_1',
        title: 'hello',
        sort: 0,
        pinned_at: null,
        cwd: '/repo',
        template_id: null,
        closed_at: null,
        created_at: 1,
        updated_at: 2,
        agent_message: 'moving to dispatch',
      },
    });
    expect(parsed.ev).toBe('track.updated');
    if (parsed.ev === 'track.updated') {
      expect(parsed.data.template_input).toBeNull();
      expect(parsed.data.plugin_scope).toBeNull();
    }
  });
});

// The zod schemas are pinned to the TS types `ts-rs` emits from the Rust `Event` enum;
// drift in either direction fails `tsc -b` here.
describe('zod ↔ ts-rs conformance', () => {
  it('keeps API and state decode failures intentionally shape-equivalent', () => {
    expectTypeOf<ApiDecodeFailure>().toEqualTypeOf<DecodeFailure>();
    expectTypeOf<Extract<WireEventDecodeResult, { status: 'failed' }>['error']>()
      .toEqualTypeOf<ApiDecodeFailure>();
  });
  it('wireEventSchema infers the generated Event union', () => {
    // Transport types use intersections; check structural equivalence in both directions.
    expectTypeOf<z.infer<typeof wireEventSchema>>().toExtend<GeneratedEvent>();
    expectTypeOf<GeneratedEvent>().toExtend<z.infer<typeof wireEventSchema>>();
  });

  it('entity sub-schemas match their generated counterparts', () => {
    expectTypeOf<z.infer<typeof areaSchema>>().toEqualTypeOf<GeneratedArea>();
    expectTypeOf<z.infer<typeof trackSchema>>().toExtend<GeneratedTrack>();
    expectTypeOf<GeneratedTrack>().toExtend<z.infer<typeof trackSchema>>();
    expectTypeOf<z.infer<typeof cardSchema>>().toEqualTypeOf<GeneratedCard>();
    expectTypeOf<z.infer<typeof overlaySchema>>().toEqualTypeOf<GeneratedOverlay>();
  });

  it('REST-only sub-schemas match their generated counterparts', () => {
    expectTypeOf<z.infer<typeof areaFolderWireSchema>>().toEqualTypeOf<GeneratedAreaFolder>();
    expectTypeOf<z.infer<typeof folderConflictSchema>>().toEqualTypeOf<GeneratedFolderConflict>();
  });
});

describe('entity sub-schema compatibility', () => {
  it('areaSchema fills kind and Area defaults when absent from a legacy event', () => {
    const parsed = areaSchema.parse({
      id: 'c1',
      name: 'n',
      color: '#fff',
      sort: 0,
      created_at: 1,
      updated_at: 2,
    });
    expect(parsed).toMatchObject({
      kind: 'user', default_template_id: null, default_cwd: null,
    });
  });
});

describe('planner harness transcript lifecycle events', () => {
  it('rejects harness.transcript.cleared with non-numeric telemetry', () => {
    const result = wireEventSchema.safeParse({
      ev: 'harness.transcript.cleared',
      data: {
        worker_session_id: 'runtime_2',
        card_id: 'card_planner_1',
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
        worker_session_id: 'runtime_2',
        card_id: 'card_planner_1',
        track_id: 'track_1',
        char_count: 9,
      },
    });
    expect(parsed.ev).toBe('harness.user_message.enqueued');
    if (parsed.ev === 'harness.user_message.enqueued') {
      expect('text' in parsed.data).toBe(false);
    }
  });
});

describe('PR4 of #136: dispatcher + task-lifecycle variants', () => {
  it('requires an explicit task notification declaration', () => {
    expect(wireEventSchema.safeParse({ ev: 'task.failed', data: {
      idempotency_key: 'attempt', reason: 'failed',
    } }).success).toBe(false);
    for (const notification of ['notify', 'suppress', 'legacy']) {
      expect(wireEventSchema.safeParse({ ev: 'task.failed', data: {
        notification, idempotency_key: 'attempt', reason: 'failed',
      } }).success).toBe(true);
    }
  });

  it('parses a task.gate_result refused for a verification target mismatch (#1727 S4 slice 4)', () => {
    const parsed = wireEventSchema.parse({
      ev: 'task.gate_result',
      data: {
        task_id: 'wv-1:impl',
        idempotency_key: 'wv-1:impl',
        passed: false,
        log_tail: 'refused: verification target mismatch (head, dirty)',
        log_path: '/data/gate-logs/wv-1:impl-g1.log',
        attempt: 1,
        status_detail: 'gate-target-mismatch',
        target: {
          kind: 'candidate',
          candidate_id: 'cand-1',
          commit_sha: 'a'.repeat(40),
          lease_id: 'lease-1',
          evidence: {
            kind: 'refused',
            cwd: '/leases/lease-1',
            before: {
              head: 'b'.repeat(40),
              dirty: [' M src/lib.rs'],
              provenance: { realpath: '/leases/lease-1', common_dir: '/repo/.git', registered: true },
            },
            reasons: ['head', 'dirty'],
          },
        },
      },
    });
    expect(parsed.ev).toBe('task.gate_result');
    if (parsed.ev === 'task.gate_result') {
      expect(parsed.data.status_detail).toBe('gate-target-mismatch');
      expect(parsed.data.failing_step).toBeUndefined();
      const target = parsed.data.target;
      expect(target?.kind).toBe('candidate');
      if (target?.kind === 'candidate') {
        expect(target.candidate_id).toBe('cand-1');
        expect(target.evidence.kind).toBe('refused');
        if (target.evidence.kind === 'refused') {
          expect(target.evidence.reasons).toEqual(['head', 'dirty']);
          expect(target.evidence.before.dirty).toEqual([' M src/lib.rs']);
          expect(target.evidence.before.provenance.registered).toBe(true);
        }
      }
    }
  });

  it('parses a task.gate_result refused because no candidate existed (#1727 S4 slice 4)', () => {
    const parsed = wireEventSchema.parse({
      ev: 'task.gate_result',
      data: {
        task_id: 'wv-1:impl',
        idempotency_key: 'wv-1:impl',
        passed: false,
        log_tail: 'refused: no candidate to verify',
        log_path: '/data/gate-logs/wv-1:impl-g1.log',
        attempt: 1,
        status_detail: 'gate-infra',
        target: { kind: 'no_candidate', reason: { kind: 'delivery_pending', delivery_id: 'del-1' } },
      },
    });
    expect(parsed.ev).toBe('task.gate_result');
    if (parsed.ev === 'task.gate_result') {
      expect(parsed.data.status_detail).toBe('gate-infra');
      const target = parsed.data.target;
      expect(target?.kind).toBe('no_candidate');
      if (target?.kind === 'no_candidate') {
        expect(target.reason).toEqual({ kind: 'delivery_pending', delivery_id: 'del-1' });
      }
    }
    // `prepare` is the one phase without a `cwd`; an unbound target is a bare reason.
    const unsampled = wireEventSchema.parse({
      ev: 'task.gate_result',
      data: {
        task_id: 'wv-1:impl',
        idempotency_key: 'wv-1:impl',
        passed: false,
        log_tail: '',
        log_path: '/data/gate-logs/wv-1:impl-g1.log',
        attempt: 1,
        status_detail: 'gate-infra',
        target: {
          kind: 'candidate',
          candidate_id: 'cand-1',
          commit_sha: 'a'.repeat(40),
          lease_id: 'lease-1',
          evidence: { kind: 'unsampled', phase: { kind: 'prepare', reason: 'git status exited 128' } },
        },
      },
    });
    expect(unsampled.ev).toBe('task.gate_result');
    // A first verdict that reuses the worker's passing gate run (#2464).
    const reused = wireEventSchema.parse({
      ev: 'task.gate_result',
      data: {
        task_id: 'wv-1:impl',
        idempotency_key: 'wv-1:impl',
        passed: true,
        log_tail: '',
        log_path: '/data/gate-logs/wv-1:impl-g1.log',
        attempt: 1,
        target: {
          kind: 'candidate',
          candidate_id: 'cand-1',
          commit_sha: 'a'.repeat(40),
          lease_id: 'lease-1',
          evidence: {
            kind: 'reused',
            run: 'wv-1:impl#r1',
            cwd: '/leases/lease-1',
            before: {
              head: 'a'.repeat(40),
              dirty: [],
              provenance: { realpath: '/leases/lease-1', common_dir: '/repo/.git', registered: true },
            },
          },
        },
      },
    });
    if (reused.ev === 'task.gate_result' && reused.data.target?.kind === 'candidate') {
      expect(reused.data.target.evidence.kind).toBe('reused');
    } else {
      throw new Error('reused gate result did not decode as a candidate target');
    }
    const unbound = wireEventSchema.safeParse({
      ev: 'task.gate_result',
      data: {
        task_id: 'wv-1:impl',
        idempotency_key: 'wv-1:impl',
        passed: true,
        log_tail: '',
        log_path: '/data/gate-logs/wv-1:impl-g1.log',
        attempt: 1,
        target: { kind: 'unbound', reason: 'legacy_lease' },
      },
    });
    expect(unbound.success).toBe(true);
    const badReason = wireEventSchema.safeParse({
      ev: 'task.gate_result',
      data: {
        task_id: 'wv-1:impl',
        idempotency_key: 'wv-1:impl',
        passed: true,
        log_tail: '',
        log_path: '/data/gate-logs/wv-1:impl-g1.log',
        attempt: 1,
        target: { kind: 'unbound', reason: 'unknown' },
      },
    });
    expect(badReason.success).toBe(false);
  });
});

describe('PR2 of #247: track.report_edited', () => {
  it('accepts every author discriminator (planner | user | kernel | plugin)', () => {
    for (const author of [
      'planner',
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
});

describe('entity sub-schemas', () => {
  it('trackSchema defaults `closed_at` to null when the field is missing', () => {
    const w = { workspace: { isolation: 'none' as const, kind: 'attached' as const, path: '', frozen_at: null },
      id: 'w1',
      area_id: 'c1',
      title: 't',
      sort: 0,
      created_at: 1,
      updated_at: 2,
    };
    expect(trackSchema.parse(w).closed_at).toBeNull();
  });

  it('trackSchema requires and preserves `workspace` (#1147 S1)', () => {
    // An undeclared field is *stripped* by zod, so the "present key" case pins that
    // the field actually survives parsing.
    const base = { workspace: { isolation: 'none' as const, kind: 'attached' as const, path: '', frozen_at: null },
      id: 'w1',
      area_id: 'c1',
      title: 't',
      sort: 0,
      created_at: 1,
      updated_at: 2,
    };
    expect(trackSchema.parse(base).workspace).toEqual({
      isolation: 'none',
      kind: 'attached',
      path: '',
      frozen_at: null,
    });
    const live = trackSchema.parse({
      ...base,
      cwd: '/srv/neige-workspaces/c1/w1',
      workspace: {
        isolation: 'managed',
        kind: 'managed',
        path: '/srv/neige-workspaces/c1/w1',
        frozen_at: 4242,
      },
    });
    expect(live.workspace).toEqual({
      isolation: 'managed',
      kind: 'managed',
      path: '/srv/neige-workspaces/c1/w1',
      frozen_at: 4242,
    });
    // No `live.workspace.path === live.cwd` assertion: zod has no cross-field constraint,
    // so it would be true no matter what the schema said.
    // #1830: an attached track's worktree rides along; serde omits it when there is none.
    const worktree = '/repo/.claude/worktrees/track-w1';
    expect(
      trackSchema.parse({
        ...base,
        workspace: { isolation: 'worktree', kind: 'attached', path: '/repo', frozen_at: 1, worktree },
      }).workspace.worktree,
    ).toBe(worktree);
    expect(live.workspace).not.toHaveProperty('worktree');
  });

  it('trackSchema rejects a present-but-incomplete `workspace` (#1147 S1)', () => {
    // Workspace and every required projection field must be present.
    const base = { workspace: { isolation: 'none' as const, kind: 'attached' as const, path: '', frozen_at: null },
      id: 'w1',
      area_id: 'c1',
      title: 't',
      sort: 0,
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
        workspace: { isolation: 'managed', kind: 'managed', path: '/p', frozen_at: null },
      }).success,
    ).toBe(true);
  });

});

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

// Historical rows spell the template fields with the pre-rename keys; this one-way
// normalize is deliberately one of three independent copies, not a shared helper.
describe('#1209 pre-rename template keys on the track shape', () => {
  const legacyTrack = { workspace: { isolation: 'none' as const, kind: 'attached' as const, path: '', frozen_at: null },
    id: 'w1',
    area_id: 'c1',
    title: 't',
    sort: 0,
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

// #1727 S4: the settlement result is a discriminated union on `kind`; both shapes are exact.
describe('git delivery settlement', () => {
  const base = {
    task_id: 'attempt', idempotency_key: 'attempt', track_id: 'track-1', card_id: 'worker',
    delivery_id: 'delivery-1', ordinal: 1,
  };
  const candidate = {
    ev: 'task.git_delivery_settled',
    data: {
      ...base,
      result: { kind: 'candidate', candidate_id: 'delivery-1', commit_sha: 'c'.repeat(40), base_sha: 'b'.repeat(40), base_is_ancestor: true },
      wake_reason: 'ungated_candidate',
    },
  };
  const failed = {
    ev: 'task.git_delivery_settled',
    data: {
      ...base,
      result: { kind: 'failed', code: 'commit_failed', reason: 'index.lock exists', retry_allowed: true },
      wake_reason: 'failed',
    },
  };

  it('decodes the candidate shape and every wake reason', () => {
    expect(wireEventSchema.parse(candidate)).toEqual(candidate);
    for (const wake_reason of ['failed', 'ungated_candidate', 'gate_already_terminal', 'deferred_to_gate']) {
      const event = { ...candidate, data: { ...candidate.data, wake_reason } };
      expect(wireEventSchema.parse(event)).toEqual(event);
    }
    expect(wireEventSchema.safeParse({ ...candidate, data: { ...candidate.data, wake_reason: 'silent' } }).success).toBe(false);
    const partial: Record<string, unknown> = { ...candidate.data.result };
    delete partial.base_is_ancestor;
    expect(wireEventSchema.safeParse({ ...candidate, data: { ...candidate.data, result: partial } }).success).toBe(false);
  });

  it('decodes the failed shape and every failure code', () => {
    expect(wireEventSchema.parse(failed)).toEqual(failed);
    for (const code of ['workspace_missing', 'provenance_mismatch', 'commit_failed', 'unresolved']) {
      const event = { ...failed, data: { ...failed.data, result: { ...failed.data.result, code } } };
      expect(wireEventSchema.parse(event)).toEqual(event);
    }
    expect(wireEventSchema.safeParse({ ...failed, data: { ...failed.data, result: { ...failed.data.result, code: 'hook_failed' } } }).success).toBe(false);
    expect(wireEventSchema.safeParse({ ...failed, data: { ...failed.data, result: { kind: 'pending' } } }).success).toBe(false);
    for (const field of ['task_id', 'idempotency_key', 'track_id', 'card_id', 'delivery_id', 'ordinal', 'result', 'wake_reason']) {
      const data: Record<string, unknown> = { ...failed.data };
      delete data[field];
      expect(wireEventSchema.safeParse({ ...failed, data }).success).toBe(false);
    }
  });
});

describe('worktree.committed delivery fields', () => {
  const legacy = {
    ev: 'worktree.committed',
    data: { track_id: 'track-1', card_id: 'worker', commit_sha: 'c'.repeat(40), branch: 'neige/track-1/worker' },
  };
  it('accepts legacy auto-commits without the #1727 fields and kernel deliveries with them', () => {
    expect(wireEventSchema.parse(legacy)).toEqual(legacy);
    const delivered = { ...legacy, data: { ...legacy.data, delivery_id: 'delivery-1', base_is_ancestor: false } };
    expect(wireEventSchema.parse(delivered)).toEqual(delivered);
    expect(wireEventSchema.safeParse({ ...legacy, data: { ...legacy.data, base_is_ancestor: 'yes' } }).success).toBe(false);
  });
});

it('accepts admin state and grant_admin asks in the event contract', () => {
  const track = trackSchema.parse({ workspace: { isolation: 'none' as const, kind: 'attached' as const, path: '', frozen_at: null }, id: 't', area_id: 'a', title: '', sort: 0, created_at: 1, updated_at: 1, admin_granted_at: 42 });
  expect(track.admin_granted_at).toBe(42);
  expect(trackSchema.parse({ ...track, admin_granted_at: undefined }).admin_granted_at).toBeNull();
  const ask = wireEventSchema.parse({ ev: 'ask.requested', data: {
    track_id: 't', card_id: 'p', questions: [{ title: 'Manage Tracks?', options: ['Grant admin', 'Not now'] }],
    delivery: 'wake', action: { kind: 'grant_admin' },
  } });
  expect(ask.ev).toBe('ask.requested');
});

it('accepts forge approval asks and refuses an incomplete or unknown ask action', () => {
  const ask = { ev: 'ask.requested', data: {
    track_id: 't', questions: [{ title: 'Re-run?', options: ['Approve', 'Decline'] }], delivery: 'wake',
    action: { kind: 'forge_action', idempotency_key: 'gitforge:t:p:gh.run.rerun_failed:v1:[]', payload_hash: 'h' },
  } };
  expect(wireEventSchema.parse(ask)).toEqual(ask);
  for (const action of [{ kind: 'forge_action', idempotency_key: 'k' }, { kind: 'unknown' }]) {
    expect(wireEventSchema.safeParse({ ...ask, data: { ...ask.data, action } }).success).toBe(false);
  }
});

it('accepts objective approval asks and requires the objective and exact standard hash', () => {
  const ask = { ev: 'ask.requested', data: {
    track_id: 't', questions: [{ title: '批准 KR？', options: ['批准', '不批准'] }], delivery: 'wake',
    action: { kind: 'objective_approval', objective: 'kr', standard_hash: 'hash' },
  } };
  expect(wireEventSchema.parse(ask)).toEqual(ask);
  for (const action of [
    { kind: 'objective_approval', objective: 'kr' },
    { kind: 'objective_approval', standard_hash: 'hash' },
    { kind: 'objective_approval', objective: 1, standard_hash: 'hash' },
  ]) {
    expect(wireEventSchema.safeParse({ ...ask, data: { ...ask.data, action } }).success).toBe(false);
  }
});
