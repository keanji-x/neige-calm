// `@` in a Planner's composer (#1881): the area's tags, reports and report blocks, as the
// server ranked them, turned into one ordered list of things to pick. The server builds every
// `insert` (the path format lives in Rust alone); this file never assembles a path.

import { z } from 'zod';

import type {
  BlockMention, MentionCandidates, TagMention, TrackMention,
} from '../api/generated/wire.js';
import type { ApiAbortSignal, ApiOperation } from '../api/types.js';

const tagMentionSchema: z.ZodType<TagMention> = z.object({
  label: z.string(),
  track_count: z.number(),
  insert: z.string(),
});

const trackMentionSchema: z.ZodType<TrackMention> = z.object({
  label: z.string(),
  track_id: z.string(),
  insert: z.string(),
});

const blockMentionSchema: z.ZodType<BlockMention> = z.object({
  label: z.string(),
  block_id: z.string(),
  track_title: z.string(),
  track_id: z.string(),
  insert: z.string(),
});

export const mentionCandidatesSchema: z.ZodType<MentionCandidates> = z.object({
  tags: z.array(tagMentionSchema),
  tracks: z.array(trackMentionSchema),
  blocks: z.array(blockMentionSchema),
});

/**
 * `q` is the text typed after `@`, sent even when empty: empty is the server's "recommend"
 * request, and the parameter is required. `trackId` is the track the message is written in,
 * whose blocks the server ranks first; `null` where there is no such track yet.
 */
export function mentionsOperation(
  areaId: string, query: string, trackId: string | null,
): ApiOperation<MentionCandidates> {
  const track = trackId === null ? '' : `&track=${encodeURIComponent(trackId)}`;
  return {
    method: 'GET',
    path: `/api/areas/${encodeURIComponent(areaId)}/mentions?q=${encodeURIComponent(query)}${track}`,
    responseSchema: mentionCandidatesSchema,
  };
}

export type MentionKind = 'tag' | 'track' | 'block';

/** One row of the `@` menu and the chip it becomes. */
export type MentionSuggestion = Readonly<{
  /** Unique across the whole list, so the three groups can share one keyed list. */
  id: string;
  kind: MentionKind;
  /** The row's name. */
  label: string;
  /** The row's secondary text, or `null` when the name says everything. */
  detail: string | null;
  /** What the chip shows in the field. */
  chip: string;
  /** The exact text the message carries for this pick, verbatim from the server. */
  insert: string;
}>;

/** The `@` source's port: the ranked suggestions for one query, abandoned when `signal` aborts. */
export type MentionSearch = (query: string, signal: ApiAbortSignal) => Promise<readonly MentionSuggestion[]>;

function tagSuggestion(tag: TagMention): MentionSuggestion {
  return {
    id: `tag:${tag.insert}`,
    kind: 'tag',
    label: `#${tag.label}`,
    detail: tag.track_count === 1 ? '1 track' : `${tag.track_count} tracks`,
    chip: `#${tag.label}`,
    insert: tag.insert,
  };
}

function trackSuggestion(track: TrackMention): MentionSuggestion {
  return {
    id: `track:${track.insert}`,
    kind: 'track',
    label: track.label,
    detail: null,
    chip: track.label,
    insert: track.insert,
  };
}

function blockSuggestion(block: BlockMention): MentionSuggestion {
  return {
    id: `block:${block.insert}`,
    kind: 'block',
    label: block.label,
    detail: block.track_title,
    chip: `${block.track_title} › ${block.label}`,
    insert: block.insert,
  };
}

/**
 * Tags, then tracks, then blocks; inside each group the server's order, which is its ranking.
 * Nothing is filtered, re-ranked or re-capped here.
 */
export function mentionSuggestionsOf(candidates: MentionCandidates): readonly MentionSuggestion[] {
  return [
    ...candidates.tags.map(tagSuggestion),
    ...candidates.tracks.map(trackSuggestion),
    ...candidates.blocks.map(blockSuggestion),
  ];
}
