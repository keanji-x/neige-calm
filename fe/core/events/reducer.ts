import type { EventFrame } from './protocol.js';
import { invalidationPlanFor, type CacheWrite, type InvalidationContext, type QueryKey } from './invalidation-plan.js';

export type EventState = Readonly<{ cursor: number | null; syncEventVersion: number | null }>;
export type EventEffect =
  | Readonly<{ type: 'persist-cursor'; id: number | null }>
  | Readonly<{ type: 'invalidate'; keys: readonly QueryKey[] | null }>
  | Readonly<{ type: 'remove'; keys: readonly QueryKey[] }>
  | Readonly<{ type: 'write-through'; writes: readonly CacheWrite[] }>
  | Readonly<{ type: 'clear-cache' }>
  | Readonly<{ type: 'reconnect' }>;
export type EventReduction = Readonly<{ state: EventState; effects: readonly EventEffect[] }>;

export function initialEventState(syncEventVersion: number | null, cursor: number | null = null): EventState {
  return { cursor, syncEventVersion };
}

function validCursor(value: number | null): value is number {
  return value !== null && value > 0;
}

function advance(state: EventState, rawId: number | null): EventReduction {
  if (!validCursor(rawId) || (state.cursor !== null && rawId <= state.cursor)) return { state, effects: [] };
  return { state: { ...state, cursor: rawId }, effects: [{ type: 'persist-cursor', id: rawId }] };
}

export function reduceEventFrame(
  state: EventState,
  frame: EventFrame,
  context?: InvalidationContext,
): EventReduction {
  if (frame.type === 'snapshot-required') {
    return {
      state: { ...state, cursor: null },
      effects: [{ type: 'persist-cursor', id: null }, { type: 'clear-cache' }, { type: 'reconnect' }],
    };
  }
  if (frame.type === 'replay-complete') {
    if (state.cursor !== null && frame.id !== null && frame.id < state.cursor) {
      return {
        state: { ...state, cursor: null },
        effects: [{ type: 'persist-cursor', id: null }, { type: 'clear-cache' }, { type: 'reconnect' }],
      };
    }
    const advanced = advance(state, frame.id);
    return { state: advanced.state, effects: [...advanced.effects, { type: 'invalidate', keys: null }] };
  }

  const eventVersion = frame.type === 'event' ? frame.meta.eventVersion : frame.eventVersion;
  if (
    state.syncEventVersion !== null && eventVersion !== null && eventVersion > state.syncEventVersion
  ) return { state, effects: [] };

  const rawId = frame.type === 'event' ? frame.meta.id : frame.id;
  const advanced = advance(state, rawId);
  if (frame.type === 'malformed-event') return advanced;

  const plan = invalidationPlanFor(frame.event, context);
  const effects: EventEffect[] = [...advanced.effects];
  if (plan.writeThrough.length > 0) effects.push({ type: 'write-through', writes: plan.writeThrough });
  if (plan.invalidate.length > 0) effects.push({ type: 'invalidate', keys: plan.invalidate });
  if (plan.remove.length > 0) effects.push({ type: 'remove', keys: plan.remove });
  return { state: advanced.state, effects };
}
