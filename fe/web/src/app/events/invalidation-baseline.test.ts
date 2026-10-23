import { QueryClient } from '@tanstack/react-query';
import { expect, it, vi } from 'vitest';

import { wireEventSchema, type WireEvent } from '../../../../core/api/schemas.ts';
import { invalidationPlanFor } from '../../../../core/events/invalidation-plan.ts';
import { applyEventEffects } from './query-invalidation-adapter.ts';

// Captured on 2c9040a6 before replacing the adapter's translation table. Keep literal output
// independent of query-key factories: changing both consumers must not rewrite this baseline.
it('preserves the event-to-cache operation baseline through production entry points', async () => {
  const rows: string[] = [];
  const kinds = wireEventSchema.options.map(schema => schema.shape.ev.value);
  for (const ev of kinds) {
    // Exercise all policy branches, including unresolved ownership and conditional harness writes.
    for (const resolved of [false, true]) {
      const client = new QueryClient();
      const calls: string[] = [];
      vi.spyOn(client, 'cancelQueries').mockImplementation(filters => { calls.push(`cancel:${JSON.stringify(filters?.queryKey)}`); return Promise.resolve(); });
      vi.spyOn(client, 'invalidateQueries').mockImplementation(filters => { calls.push(`invalidate:${JSON.stringify(filters?.queryKey)}`); return Promise.resolve(); });
      vi.spyOn(client, 'removeQueries').mockImplementation(filters => { calls.push(`remove:${JSON.stringify(filters?.queryKey)}`); });
      vi.spyOn(client, 'resetQueries').mockImplementation(filters => { calls.push(`reset:${JSON.stringify(filters?.queryKey)}`); return Promise.resolve(); });
      // Deliberately use only the named fields a policy consumes. Task ids stay opaque;
      // git_delivery_settled alone has an explicit track_id in its released wire contract.
      const data = {
        id: 'entity', area_id: 'area', plugin_id: 'plugin',
        ...(ev.startsWith('task.') && ev !== 'task.git_delivery_settled' ? {} : { card_id: 'card' }),
        ...(ev.startsWith('task.') && ev !== 'task.git_delivery_settled' ? {} : { track_id: 'track' }),
        entity_kind: resolved ? 'track' : 'card', entity_id: 'entity',
        ...(resolved ? { agent_message: 'canceled', new_phase: 'turn_completed', change: 'restored',
          method: 'item/completed', item_type: 'userMessage' } : {}),
      };
      const plan = invalidationPlanFor({ ev, data } as WireEvent,
        { findTrackOwningCard: () => resolved ? 'track' : null });
      applyEventEffects(client, [
        { type: 'write-through', writes: plan.writeThrough },
        { type: 'invalidate', keys: plan.invalidate },
        { type: 'remove', keys: plan.remove },
      ]);
      await Promise.resolve();
      for (const call of calls.filter(call => call.startsWith('cancel:'))) {
        const invalidation = call.replace('cancel:', 'invalidate:');
        if (calls.includes(invalidation)) expect(calls.indexOf(call)).toBeLessThan(calls.indexOf(invalidation));
      }
      rows.push(`${ev}/${resolved ? 'resolved' : 'unresolved'} ${JSON.stringify(calls.sort())}`);
      client.clear();
    }
  }
  expect(rows).toMatchSnapshot();
});
