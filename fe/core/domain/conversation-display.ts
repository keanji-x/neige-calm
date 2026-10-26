/** One typed read contract over the existing transcript resource. Display identities are not native turns. */
import { z } from 'zod';
import type { ApiOperation } from '../api/types.js';
import { agentProviderSchema, conversationCursorSchema, workerSessionStateSchema } from '../api/schemas.js';
import type { ConversationCursor, ConversationDisplayItem, ConversationPage, ConversationPendingInput, HarnessItem as TranscriptRow } from '../api/generated/wire.js';
import { plannerPermissionModeSchema } from './planner-permission-mode.js';
import { SYSTEM_PRESENTATION_LABELS, systemInputPresentationSchema, buildTranscript, retireFollowedThoughts, harnessTimeoutEvidenceSchema, harnessItemSchema as transcriptRowSchema, HARNESS_ITEMS_PAGE_LIMIT as TRANSCRIPT_PAGE_LIMIT, plannerRunOperation, type PlannerRun, type TranscriptEntry } from './conversation.js';

const decimalU64 = z.string().regex(/^(0|[1-9]\d*)$/).pipe(z.string().refine(value => BigInt(value) <= 18446744073709551615n));
const keepResultSchema = z.object({ carrier_id:z.string(), submission_id:z.string(), attempt_id:z.string(),
  revision:decimalU64, waiting_for_settlement:z.boolean() }).strict();

export function keepConversationInputOperation(card: string, args: import('../api/generated/wire.js').KeepConversationInputRequest, key: string): ApiOperation<import('../api/generated/wire.js').KeepConversationInputResult> {
  return { method:'POST', path:`/api/cards/${encodeURIComponent(card)}/conversation/input/keep`,
    headers:{'Idempotency-Key':key}, body:args, responseSchema:keepResultSchema.refine(result =>
      result.carrier_id === args.carrier_id && result.submission_id === args.submission_id
      && result.attempt_id === args.attempt_id && result.revision === (BigInt(args.revision)+1n).toString(),
    'Keep receipt does not match its original CAS request') };
}

const retryResultSchema = z.object({...keepResultSchema.shape,retry_intent_id:z.string().min(1)}).strict();
export function retryConversationInputOperation(card: string, args: import('../api/generated/wire.js').RetryConversationInputRequest, key: string): ApiOperation<import('../api/generated/wire.js').RetryConversationInputResult> {
  return { method:'POST', path:`/api/cards/${encodeURIComponent(card)}/conversation/input/retry`,
    headers:{'Idempotency-Key':key}, body:args, responseSchema:retryResultSchema.refine(result =>
      result.carrier_id === args.carrier_id && result.submission_id === args.submission_id
      && result.attempt_id === args.attempt_id && result.revision === (BigInt(args.revision)+1n).toString(),
    'Retry receipt does not match its original CAS request') };
}
const availabilitySchema = z.discriminatedUnion('kind', [
  z.object({kind:z.literal('available')}).strict(),
  z.object({kind:z.literal('unavailable'),reason:z.enum(['already_kept','already_resolved','not_held','prior_invocation_not_recorded',
    'invocation_not_fully_frozen','attachments_not_frozen','erased_or_withdrawn','unsupported_binding','unsupported_source'])}).strict(),
]);

const attachment = z.object({ id: z.string(), size: z.number(), url: z.string(), contentType:z.string() });
const importedProvenance = z.object({kind:z.literal('unknown_legacy'),original_row_id:z.number().int().positive(),
  worker_session_id:z.string(),card_id:z.string(),track_id:z.string(),thread_id:z.string(),
  turn_id:z.string().nullable(),item_uuid:z.string().nullable(),item_type:z.string().nullable(),method:z.string(),
  created_at_ms:z.number(),original_digest:z.string().regex(/^sha256-v1:[0-9a-f]{64}$/),
  import_boundary_sequence:z.number().int().nonnegative()}).strict();
const identity = z.discriminatedUnion('kind', [
  z.object({ kind: z.literal('human'), message_id: z.string() }).strict(),
  z.object({ kind: z.literal('system'), observation_id: z.string() }).strict(),
  z.object({ kind: z.literal('native'), thread_id: z.string(), item_id: z.string() }).strict(),
  z.object({ kind:z.literal('imported'),provenance:importedProvenance }).strict(),
]);
export const conversationPageSchema: z.ZodType<ConversationPage> = z.object({
  contract: z.literal('conversation_v1'), source: z.enum(['presentation', 'historical_raw']),
  items: z.array(z.object({ cursor: conversationCursorSchema, source: z.discriminatedUnion('kind', [
    z.object({ kind: z.literal('human'), message_id: z.string(), revision: z.number().int().positive(),
      author: z.object({ user_id: z.string(), display_name: z.string() }).strict(), text: z.string(),
      attachments: z.array(attachment), created_at_ms: z.number() }).strict(),
    z.object({ kind: z.literal('system'), observation_id: z.string(), presentation: systemInputPresentationSchema,
      text: z.string(), created_at_ms: z.number() }).strict(),
    z.object({ kind: z.literal('native'), item: transcriptRowSchema }).strict(),
    z.object({ kind:z.literal('imported_item'),item:transcriptRowSchema,provenance:importedProvenance }).strict(),
    z.object({ kind: z.literal('historical_raw'), item: transcriptRowSchema }).strict(),
    z.object({ kind: z.literal('removed'), identity }).strict(),
  ]) }).strict()), next_cursor: conversationCursorSchema.nullable(),
}).strict();

export function conversationItemsOperation(card: string, cursor: ConversationCursor | null): ApiOperation<ConversationPage> {
  return { method:'GET', path:`/api/cards/${encodeURIComponent(card)}/harness/items?contract=conversation_v1&direction=desc&limit=${TRANSCRIPT_PAGE_LIMIT}${cursor === null ? '' : `&cursor=${encodeURIComponent(JSON.stringify(cursor))}`}`,
    responseSchema:conversationPageSchema.refine(page => page.items.every(item => item.cursor.card_id === card
      && item.cursor.source === page.source
      && (item.source.kind !== 'imported_item' || (item.source.provenance.card_id === card
        && item.source.item.card_id === card && item.source.provenance.original_row_id === item.source.item.id
        && item.cursor.position > item.source.provenance.import_boundary_sequence))
      && (item.source.kind !== 'removed' || item.source.identity.kind !== 'imported'
        || (item.source.identity.provenance.card_id === card
          && item.cursor.position > item.source.identity.provenance.import_boundary_sequence))
      && (page.source === 'historical_raw'
        ? item.source.kind === 'historical_raw' : item.source.kind !== 'historical_raw'))
      && (page.next_cursor === null || (page.next_cursor.card_id === card && page.next_cursor.source === page.source)),
      'Conversation cursor/source scope mismatch') };
}

/** Only real Native/raw observations may retire a streamed native item. */
export function nativeDisplayRows(items: readonly ConversationDisplayItem[]): readonly TranscriptRow[] {
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
    if (source.kind === 'system') return [{
      id:`system:${source.observation_id}`, author:'system', label:SYSTEM_PRESENTATION_LABELS[source.presentation],
      text:source.text, atMs:source.created_at_ms, displayPosition:cursor.position,
    }];
    if (source.kind === 'human') return [{
      id:`human:${source.message_id}`, author:source.author.user_id === viewerId ? 'you' : 'human',
      human:{ userId:source.author.user_id, displayName:source.author.display_name, messageId:source.message_id, revision:source.revision },
      text:source.text, attachments:source.attachments, atMs:source.created_at_ms, displayPosition:cursor.position,
    }];
    if (source.kind === 'imported_item') return buildTranscript([source.item]).map((entry,index) => {
      const position={id:`imported:${source.provenance.original_row_id}:${index}`,displayPosition:cursor.position};
      if(entry.author === 'system')return {...entry,...position,label:`Imported history · ${entry.label}`};
      if(entry.author === 'activity')return {...entry,...position,verb:`Imported history · ${entry.verb}`};
      if(entry.author === 'turn')return {...entry,...position,imported:true};
      // Native folded segments can carry queue-looking hints, never application acceptance.
      return { text:entry.text,atMs:entry.atMs,attachments:entry.attachments, author:entry.author === 'you' || entry.author === 'human' ? 'unknown' as const : entry.author,
        imported:true,...position };
    });
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
  pending_inputs:z.array(z.discriminatedUnion('kind',[
    z.object({kind:z.literal('explicit_successor'),retry_intent_id:z.string().min(1),queue_position:z.number().int().nonnegative(),queued_at_ms:z.number()}).strict(),
    z.object({kind:z.literal('human'),message_id:z.string(),revision:z.number().int().positive(),
      queue_position:z.number().int().nonnegative(),queued_at_ms:z.number()}).strict(),
    z.object({kind:z.literal('system'),observation_id:z.string(),
      queue_position:z.number().int().nonnegative(),queued_at_ms:z.number()}).strict(),
  ])),
  active_input:z.object({ current_execution_id:z.string().nullable(),submissions:z.array(z.object({
    carrier_id:z.string().min(1),logical_turn_id:z.string().min(1),submission_id:z.string().min(1),
    attempt:z.object({
      attempt_id:z.string().min(1),revision:decimalU64,
      target:z.object({provider:agentProviderSchema,thread_id:z.string().min(1),
        native_session_id:z.string().nullable(),local_turn_id:z.string().nullable(),native_turn_id:z.string().nullable(),rpc_request_id:decimalU64.nullable()}).strict(),
      delivery:z.enum(['crossing_unknown','unknown','not_dispatched','accepted']),
      invocation:z.discriminatedUnion('kind',[
        z.object({kind:z.literal('recorded'),configuration:z.object({model:z.string().nullable(),effort:z.string().nullable(),permission:plannerPermissionModeSchema}).strict()}).strict(),
        z.object({kind:z.literal('prior_invocation_not_recorded')}).strict(),
      ]),
      resolution:z.discriminatedUnion('kind',[
        z.object({kind:z.literal('keep'),result:keepResultSchema}).strict(),
        z.object({kind:z.literal('retry'),result:retryResultSchema}).strict(),
      ]).nullable(),
      actions:z.object({keep:availabilitySchema,retry:availabilitySchema}).strict(),
    }).strict().nullable(),
  }).strict()) }).strict().nullable(),
  native_turn_id:z.string().nullable(),
}).refine(run=>run.input_contract !== 'canonical' || run.runtime_state !== null,
  'Canonical runs require their actual current runtime state');

/** Queue references retain their actual kind and version, independently of the loaded body page. */
export function orderedPendingInputs(inputs: readonly ConversationPendingInput[]): readonly ConversationPendingInput[] {
  return [...inputs].sort((left,right)=>left.queue_position-right.queue_position);
}

export function conversationRunOperation(card: string): ApiOperation<PlannerRun & z.infer<typeof conversationRunMetadataSchema>> {
  const original = plannerRunOperation(card);
  return { ...original, path:`${original.path}?contract=conversation_v1`,
    responseSchema:original.responseSchema.and(conversationRunMetadataSchema) };
}
