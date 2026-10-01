// Mention candidates: ranked Area report references and plugin catalog documentation.
// Report addresses come from the server; plugin reference text is built from catalog metadata.

import { z } from 'zod';
import type { PluginListItem } from './plugins.js';

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
 * `query` is `MentionQuery.text` (what was typed after `@`, less any type prefix), sent even
 * when empty: empty is the server's "recommend" request, and the parameter is required.
 * `trackId` is the track the message is written in, whose blocks the server ranks first; `null`
 * where there is no such track yet.
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

export type MentionKind = 'tag' | 'track' | 'block' | 'plugin';

/** One row of the `@` menu and the chip it becomes. */
export type MentionSuggestion = Readonly<{
  /** Unique across the whole list, so all groups can share one keyed list. */
  id: string;
  kind: MentionKind;
  /** The row's name. */
  label: string;
  /** The row's secondary text, or `null` when the name says everything. */
  detail: string | null;
  /** What the chip shows in the field. */
  chip: string;
  /** The exact text the message carries for this pick, from its candidate producer. */
  insert: string;
}>;

/** The `@` source's port: the ranked suggestions for the text typed after `@`, type prefix included, abandoned when `signal` aborts. */
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
 * What the text typed after `@` asks for. A leading `+`, `#`, `/` or `>` narrows the menu to plugins,
 * tags, tracks or blocks (`kind`) and is not part of the search: `text` is the rest, the server's `q`.
 * Without one of them `kind` is `null` and `text` is everything typed, so a name that starts
 * with one of these characters is still found by typing it without that character.
 */
export type MentionQuery = Readonly<{ kind: MentionKind | null; text: string }>;

/** Also accepts a Chinese IME's punctuation: `＋`, `＃`, `、` and `》`. */
function prefixKind(char: string): MentionKind | null {
  switch (char) {
    case '+': case '＋': return 'plugin';
    case '#': case '＃': return 'tag';
    case '/': case '、': return 'track';
    case '>': case '》': return 'block';
    default: return null;
  }
}

export function mentionQueryOf(typed: string): MentionQuery {
  const kind = prefixKind(typed.charAt(0));
  return kind === null ? { kind, text: typed } : { kind, text: typed.slice(1) };
}

/**
 * For typed text: tags, then tracks, then blocks. For an empty `query.text` the order is
 * reversed: the server puts the current track's blocks first among its recommendations, and
 * they are the likeliest pick. Inside each group the server's order, which is its ranking;
 * nothing is re-ranked or re-capped here. A `query.kind` keeps only that group: the server caps
 * each group on its own, so this is the list a server-side filter would give.
 */
export function mentionSuggestionsOf(candidates: MentionCandidates, query: MentionQuery): readonly MentionSuggestion[] {
  const tags = candidates.tags.map(tagSuggestion);
  const tracks = candidates.tracks.map(trackSuggestion);
  const blocks = candidates.blocks.map(blockSuggestion);
  const all = query.text === '' ? [...blocks, ...tracks, ...tags] : [...tags, ...tracks, ...blocks];
  return query.kind === null ? all : all.filter((suggestion) => suggestion.kind === query.kind);
}

/** A persisted message keeps the server address, not the composer's transient chip metadata. */
export type SentMentionPart = Readonly<{ text: string; label: string | null }>;

function sentMentionLabel(address: string): string | null {
  if (address.startsWith('tag:')) {
    const tag = address.slice(4);
    // Stored tags obey report_tags::normalize_tag; malformed prefixes must not consume later picks.
    if (tag === '' || /[\s,\p{Cc}]/u.test(tag) || Array.from(tag).length > 64) return null;
    return `#${tag}`;
  }
  const report = /^area\/reports\/(.+)\.md(?:#([^\s]+))?$/.exec(address);
  if (report === null) return null;
  let name: string;
  try { name = decodeURIComponent(report[1]); } catch { return null; }
  // Block headings are not persisted in the message. Keep its stable ID visible instead.
  return report[2] === undefined ? name : `${name} › ${report[2]}`;
}

/**
 * Recover display pills without fetching today's candidates or changing the sent text.
 * Match complete backtick runs, including the longer fences and edge padding emitted by
 * the server. Unknown addresses and unfinished spans remain literal user text.
 */
export function sentMentionParts(text: string): readonly SentMentionPart[] {
  const parts: SentMentionPart[] = [];
  const openings = /@(`+)/g;
  let end = 0;
  for (let opening = openings.exec(text); opening !== null; opening = openings.exec(text)) {
    const start = openings.lastIndex;
    const fences = /`+/g;
    fences.lastIndex = start;
    let closing = fences.exec(text);
    while (closing !== null && closing[0].length !== opening[1].length) closing = fences.exec(text);
    if (closing === null) continue;
    let address = text.slice(start, closing.index);
    if (address.startsWith(' ') && address.endsWith(' ')) address = address.slice(1, -1);
    const label = sentMentionLabel(address);
    if (label === null) continue;
    openings.lastIndex = fences.lastIndex;
    if (opening.index > end) parts.push({ text: text.slice(end, opening.index), label: null });
    end = fences.lastIndex;
    parts.push({ text: text.slice(opening.index, end), label });
  }
  if (end < text.length) parts.push({ text: text.slice(end), label: null });
  return parts;
}

/** References are ordinary message text, never plugin enablement or tool authorization. */
export function pluginMentionSuggestions(plugins: readonly PluginListItem[], typed: string): readonly MentionSuggestion[] {
  const query = mentionQueryOf(typed);
  if (query.kind !== null && query.kind !== 'plugin') return [];
  const needle = query.text.trim().toLocaleLowerCase();
  return plugins.filter(plugin => `${plugin.manifest_name} ${plugin.id}`.toLocaleLowerCase().includes(needle))
    .sort((a, b) => a.manifest_name.localeCompare(b.manifest_name) || a.id.localeCompare(b.id))
    .slice(0, 8).map(plugin => {
      const description = Array.from(plugin.manifest_description?.trim() || 'No description provided.').slice(0, 480).join('');
      return {
        id: `plugin:${plugin.id}`, kind: 'plugin' as const, label: plugin.manifest_name,
        chip: plugin.manifest_name, detail: description,
        insert: `Plugin reference (documentation only; kernel permissions still apply): ${JSON.stringify({ id: plugin.id, name: plugin.manifest_name, description })}`,
      };
    });
}
