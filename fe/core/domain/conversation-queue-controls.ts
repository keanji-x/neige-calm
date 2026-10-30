import { z } from 'zod';
import type { ApiOperation } from '../api/types.js';
import { deletePlannerInputOperation, editPlannerInputOperation, isUserConversationTurn, type PlannerInputMutation, type TranscriptEntry } from './conversation.js';

const messagePath = (card: string, message: string) => `/api/cards/${encodeURIComponent(card)}/conversation/messages/${encodeURIComponent(message)}`;

/** Canonical message identity uses the existing acknowledged queued-edit view. */
export function editQueuedMessageOperation(card: string, carrier: string, message: string, revision: number, text: string): ApiOperation<PlannerInputMutation & { text: string }> {
  return { ...editPlannerInputOperation(card,message,text,revision), path:messagePath(card,message),
    body:{carrier_id:carrier,if_revision:revision,text} };
}

export function withdrawQueuedMessageOperation(card: string, carrier: string, message: string, revision: number): ApiOperation<PlannerInputMutation> {
  return { ...deletePlannerInputOperation(card,message,revision), path:messagePath(card,message),
    body:{carrier_id:carrier,if_revision:revision} };
}

export function steerQueuedMessageOperation(card: string, carrier: string, message: string, revision: number, turn: string) {
  return { method:'POST' as const, path:`${messagePath(card,message)}/steer`,
    body:{carrier_id:carrier,if_revision:revision,turn_id:turn},
    responseSchema:z.object({submission_id:z.string().min(1),turn_id:z.literal(turn),
      delivery:z.enum(['accepted','not_dispatched','unknown'])}).strict(),
  };
}

/** Count each canonical pending identity once, whether listed or reference-only. */
export function unlistedPendingInputCount(inputs: readonly import('../api/generated/wire.js').ConversationPendingInput[], entries: readonly import('./conversation.js').PendingQueueEntry[]): number {
  return inputs.filter(input => input.kind !== 'human' || !entries.some(entry => entry.entry_id === input.message_id && entry.rev >= input.revision)).length;
}

/** A newer immutable Human version updates an existing listed row, never invents pending work. */
export function pendingQueueWithCurrentHumanVersions(entries: readonly import('./conversation.js').PendingQueueEntry[], turns: readonly import('./conversation.js').ConversationMessage[]): readonly import('./conversation.js').PendingQueueEntry[] {
  let changed=false;
  const result=entries.map(entry=>{
    const source=turns.find(turn=>turn.author === 'you' && turn.human?.messageId === entry.entry_id
      && turn.human.revision > entry.rev);
    if(source === undefined || source.author === 'system' || source.human === undefined)return entry;
    changed=true;
    return {...entry,rev:source.human.revision,text:source.text};
  });
  return changed ? result : entries;
}

/** A listed queue version owns older-or-equal transcript carriers of that same immutable message. */
export function pendingQueueOwnsHumanEntry(entry: TranscriptEntry, versions: ReadonlyMap<string,number>): boolean {
  if(!isUserConversationTurn(entry) || entry.human === undefined)return false;
  const queued=versions.get(entry.human.messageId);
  return queued !== undefined && queued >= entry.human.revision;
}
