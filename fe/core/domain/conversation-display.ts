/** One typed read contract over the existing transcript resource. Display identities are not native turns. */
import { z } from 'zod';
import type { ApiOperation } from '../api/types.js';
import { conversationCursorSchema, workerSessionStateSchema } from '../api/schemas.js';
import type { ConversationCursor, ConversationDisplayItem, ConversationPage, HarnessItem } from '../api/generated/wire.js';
import { buildTranscript, retireFollowedThoughts, harnessTimeoutEvidenceSchema, harnessItemSchema, HARNESS_ITEMS_PAGE_LIMIT, plannerRunOperation, type PlannerRun, type TranscriptEntry } from './conversation.js';

const attachment = z.object({ id: z.string(), size: z.number(), url: z.string(), contentType:z.string() });
const identity = z.discriminatedUnion('kind', [
  z.object({ kind: z.literal('human'), message_id: z.string() }).strict(),
  z.object({ kind: z.literal('native'), thread_id: z.string(), item_id: z.string() }).strict(),
]);
export const conversationPageSchema: z.ZodType<ConversationPage> = z.object({
  contract: z.literal('conversation_v1'), source: z.enum(['presentation', 'historical_raw']),
  items: z.array(z.object({ cursor: conversationCursorSchema, source: z.discriminatedUnion('kind', [
    z.object({ kind: z.literal('human'), message_id: z.string(), revision: z.number().int().positive(),
      author: z.object({ user_id: z.string(), display_name: z.string() }).strict(), text: z.string(),
      attachments: z.array(attachment), created_at_ms: z.number() }).strict(),
    z.object({ kind: z.literal('native'), item: harnessItemSchema }).strict(),
    z.object({ kind: z.literal('historical_raw'), item: harnessItemSchema }).strict(),
    z.object({ kind: z.literal('removed'), identity }).strict(),
  ]) }).strict()), next_cursor: conversationCursorSchema.nullable(),
}).strict();

export function conversationItemsOperation(card: string, cursor: ConversationCursor | null): ApiOperation<ConversationPage> {
  return { method:'GET', path:`/api/cards/${encodeURIComponent(card)}/harness/items?contract=conversation_v1&direction=desc&limit=${HARNESS_ITEMS_PAGE_LIMIT}${cursor === null ? '' : `&cursor=${encodeURIComponent(JSON.stringify(cursor))}`}`,
    responseSchema:conversationPageSchema.refine(page => page.items.every(item => item.cursor.card_id === card
      && item.cursor.source === page.source && (page.source === 'historical_raw'
        ? item.source.kind === 'historical_raw' : item.source.kind !== 'historical_raw'))
      && (page.next_cursor === null || (page.next_cursor.card_id === card && page.next_cursor.source === page.source)),
      'Conversation cursor/source scope mismatch') };
}

/** Only real Native/raw observations may retire a streamed native item. */
export function nativeDisplayRows(items: readonly ConversationDisplayItem[]): readonly HarnessItem[] {
  return items.flatMap(({ source }) => source.kind === 'native' || source.kind === 'historical_raw' ? [source.item] : []);
}
export function removedHumanIds(items: readonly ConversationDisplayItem[]): ReadonlySet<string> {
  return new Set(items.flatMap(({ source }) => source.kind === 'removed' && source.identity.kind === 'human'
    ? [source.identity.message_id] : []));
}

export function buildConversationTranscript(items: readonly ConversationDisplayItem[], viewerId: string | null): readonly TranscriptEntry[] {
  const ordered = [...items].sort((a,b) => a.cursor.position-b.cursor.position);
  if (ordered.every(item => item.cursor.source === 'historical_raw')) {
    return buildTranscript(nativeDisplayRows(ordered)).map(entry => entry.author === 'you'
      ? { ...entry, author:'unknown' as const } : entry);
  }
  const entries=ordered.flatMap(({ source, cursor }): readonly TranscriptEntry[] => {
    if (source.kind === 'removed') return [];
    if (source.kind === 'human') return [{
      id:`human:${source.message_id}`, author:source.author.user_id === viewerId ? 'you' : 'human',
      human:{ userId:source.author.user_id, displayName:source.author.display_name, messageId:source.message_id, revision:source.revision },
      text:source.text, attachments:source.attachments, atMs:source.created_at_ms, displayPosition:cursor.position,
    }];
    if (source.kind !== 'native') throw new Error('Mixed historical/native display contract');
    // The native parser owns provider shapes. A presentation has already paired started/completed;
    // never re-sort by physical row ID or pair equal item IDs from different native threads.
    return buildTranscript([source.item]).map((entry,index) => ({ ...entry,
      id:`native:${encodeURIComponent(JSON.stringify([source.item.thread_id,source.item.item_uuid]))}:${index}`,
      displayPosition:cursor.position,
    }));
  });
  return retireFollowedThoughts(entries);
}

export const conversationRunMetadataSchema = z.object({
  input_contract:z.enum(['canonical','historical']),
  runtime_state:workerSessionStateSchema.nullable(),
  history_replacement_supported:z.boolean(),
  terminal_timeout:harnessTimeoutEvidenceSchema.nullable(),
  pending_messages:z.array(z.object({ message_id:z.string(),revision:z.number().int().positive(),
    queue_position:z.number().int().nonnegative(),queued_at_ms:z.number() }).strict()),
  active_input:z.object({ logical_turn_id:z.string(),submissions:z.array(z.object({
    submission_id:z.string(),attempt_id:z.string().nullable(),
    delivery:z.enum(['crossing_unknown','unknown','not_dispatched','accepted']).nullable(),
  }).strict()) }).strict().nullable(),
  native_turn_id:z.string().nullable(),
}).refine(run=>run.input_contract !== 'canonical' || run.runtime_state !== null,
  'Canonical runs require their actual current runtime state');

export function conversationRunOperation(card: string): ApiOperation<PlannerRun & z.infer<typeof conversationRunMetadataSchema>> {
  const original = plannerRunOperation(card);
  return { ...original, path:`${original.path}?contract=conversation_v1`,
    responseSchema:original.responseSchema.and(conversationRunMetadataSchema) };
}
