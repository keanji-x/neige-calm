import { describe, expect, it } from 'vitest';
import { trackSchema, trackUpdatedSchema } from '../api/schemas.js';
import { toTrack, trackWireSchema } from './track.js';

const row = Object.freeze({ id: 't', area_id: 'a', title: '', sort: 0, cwd: '/repo', created_at: 1, updated_at: 1 });

describe('required workspace isolation', () => {
  it.each(['managed', 'worktree', 'none'])('preserves %s in HTTP, event and domain readers without inferring from worktree', (isolation) => {
    const wire = { ...row, workspace: { isolation, kind: 'attached', path: '/repo', frozen_at: null } };
    expect(trackSchema.parse(wire).workspace).toMatchObject({ isolation });
    expect(trackUpdatedSchema.parse({ ev: 'track.updated', data: wire }).data.workspace).toMatchObject({ isolation });
    expect(toTrack(trackWireSchema.parse(wire))).toMatchObject({ isolation });
  });

  it.each([undefined, { kind: 'attached', path: '/repo', frozen_at: null },
    { isolation: 'view', kind: 'attached', path: '/repo', frozen_at: null }])('rejects missing or unknown isolation: %j', (workspace) => {
    const wire = { ...row, workspace };
    expect(trackSchema.safeParse(wire).success).toBe(false);
    expect(trackUpdatedSchema.safeParse({ ev: 'track.updated', data: wire }).success).toBe(false);
    expect(trackWireSchema.safeParse(wire).success).toBe(false);
  });
});
