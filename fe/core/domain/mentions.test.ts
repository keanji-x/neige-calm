import { describe, expect, it } from 'vitest';

import type { MentionCandidates } from '../api/generated/wire.js';
import {
  mentionCandidatesSchema, mentionQueryOf, mentionSuggestionsOf, mentionsOperation, type MentionQuery,
} from './mentions.js';

const CANDIDATES: MentionCandidates = {
  tags: [
    { label: '部署', track_count: 3, insert: '@`tag:部署`' },
    { label: 'infra', track_count: 1, insert: '@`tag:infra`' },
  ],
  tracks: [
    { label: 'Deploy notes', track_id: 't1', insert: '@`area/reports/Deploy notes.md`' },
  ],
  blocks: [
    {
      label: 'Rollback', block_id: 'b_1a2b', track_title: 'Deploy notes', track_id: 't1',
      insert: '@`area/reports/Deploy notes.md#b_1a2b`',
    },
  ],
};

describe('mentionsOperation', () => {
  it('always sends q, even empty, and the track hint only when there is one', () => {
    expect(mentionsOperation('a 1', '', null).path).toBe('/api/areas/a%201/mentions?q=');
    expect(mentionsOperation('a1', 'dé ploy&x', 't/1').path)
      .toBe('/api/areas/a1/mentions?q=d%C3%A9%20ploy%26x&track=t%2F1');
    expect(mentionsOperation('a1', 'x', null).method).toBe('GET');
  });

  it('decodes the three groups and rejects a response missing one', () => {
    expect(mentionCandidatesSchema.parse(CANDIDATES)).toEqual(CANDIDATES);
    expect(mentionCandidatesSchema.safeParse({ tags: [], tracks: [] }).success).toBe(false);
  });
});

/** Plain `@` text: no type prefix. */
const all = (text: string): MentionQuery => ({ kind: null, text });

describe('mentionQueryOf', () => {
  it.each([
    ['#dep', { kind: 'tag', text: 'dep' }],
    ['/dep', { kind: 'track', text: 'dep' }],
    ['>roll', { kind: 'block', text: 'roll' }],
  ] as const)('reads %s as one group and strips the prefix from the search', (typed, query) => {
    expect(mentionQueryOf(typed)).toEqual(query);
  });

  it.each([
    ['＃dep', { kind: 'tag', text: 'dep' }],
    ['、dep', { kind: 'track', text: 'dep' }],
    ['》roll', { kind: 'block', text: 'roll' }],
    ['＃', { kind: 'tag', text: '' }],
    ['、', { kind: 'track', text: '' }],
    ['》', { kind: 'block', text: '' }],
  ] as const)('reads %s, a Chinese IME\'s punctuation for the prefix, the same way', (typed, query) => {
    expect(mentionQueryOf(typed)).toEqual(query);
  });

  it('reads a prefix alone as that group\'s recommendations', () => {
    expect(mentionQueryOf('#')).toEqual({ kind: 'tag', text: '' });
    expect(mentionQueryOf('/')).toEqual({ kind: 'track', text: '' });
    expect(mentionQueryOf('>')).toEqual({ kind: 'block', text: '' });
  });

  it('keeps everything typed, in all three groups, without a prefix; only the first character is one', () => {
    expect(mentionQueryOf('')).toEqual({ kind: null, text: '' });
    expect(mentionQueryOf('dep')).toEqual({ kind: null, text: 'dep' });
    expect(mentionQueryOf('a/b#c>d')).toEqual({ kind: null, text: 'a/b#c>d' });
    expect(mentionQueryOf('//x')).toEqual({ kind: 'track', text: '/x' });
  });
});

describe('mentionSuggestionsOf', () => {
  it('lists tags, then tracks, then blocks for a typed query, in the server order, each carrying its insert verbatim', () => {
    expect(mentionSuggestionsOf(CANDIDATES, all('dep'))).toEqual([
      { id: 'tag:@`tag:部署`', kind: 'tag', label: '#部署', detail: '3 tracks', chip: '#部署', insert: '@`tag:部署`' },
      { id: 'tag:@`tag:infra`', kind: 'tag', label: '#infra', detail: '1 track', chip: '#infra', insert: '@`tag:infra`' },
      {
        id: 'track:@`area/reports/Deploy notes.md`', kind: 'track', label: 'Deploy notes', detail: null,
        chip: 'Deploy notes', insert: '@`area/reports/Deploy notes.md`',
      },
      {
        id: 'block:@`area/reports/Deploy notes.md#b_1a2b`', kind: 'block', label: 'Rollback',
        detail: 'Deploy notes', chip: 'Deploy notes › Rollback', insert: '@`area/reports/Deploy notes.md#b_1a2b`',
      },
    ]);
  });

  it('puts blocks first, then tracks, then tags, for bare @', () => {
    expect(mentionSuggestionsOf(CANDIDATES, all('')).map((suggestion) => suggestion.kind))
      .toEqual(['block', 'track', 'tag', 'tag']);
    expect(mentionSuggestionsOf(CANDIDATES, all('')).map((suggestion) => suggestion.label))
      .toEqual(['Rollback', 'Deploy notes', '#部署', '#infra']);
  });

  it('is empty for an area with nothing to mention', () => {
    expect(mentionSuggestionsOf({ tags: [], tracks: [], blocks: [] }, all(''))).toEqual([]);
  });

  it('keeps ids unique when a tag and a report would otherwise collide', () => {
    const ids = mentionSuggestionsOf({
      tags: [{ label: 'x', track_count: 1, insert: '@`x`' }],
      tracks: [{ label: 'x', track_id: 't', insert: '@`x`' }],
      blocks: [],
    }, all('x')).map((suggestion) => suggestion.id);
    expect(new Set(ids).size).toBe(2);
  });

  it.each([
    ['#dep', ['tag', 'tag']],
    ['/dep', ['track']],
    ['>roll', ['block']],
    ['#', ['tag', 'tag']],
    ['/', ['track']],
    ['>', ['block']],
  ] as const)('keeps only the prefix\'s group for %s', (typed, kinds) => {
    expect(mentionSuggestionsOf(CANDIDATES, mentionQueryOf(typed)).map((suggestion) => suggestion.kind)).toEqual(kinds);
  });

  it('keeps the chip and the insert of a row found through a prefix', () => {
    expect(mentionSuggestionsOf(CANDIDATES, mentionQueryOf('>roll'))).toEqual(
      mentionSuggestionsOf(CANDIDATES, all('roll')).filter((suggestion) => suggestion.kind === 'block'),
    );
  });
});
