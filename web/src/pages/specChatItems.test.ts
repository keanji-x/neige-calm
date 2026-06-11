import { describe, expect, it } from 'vitest';
import type { HarnessItem } from '../api/generated-events';
import { parseHarnessItem } from './specChatItems';

function harnessRow(
  overrides: Partial<Omit<HarnessItem, 'params'>> & { params?: unknown } = {},
): HarnessItem {
  const { params: paramsOverride, ...rowOverrides } = overrides;
  const params =
    typeof paramsOverride === 'string'
      ? paramsOverride
      : JSON.stringify(
          paramsOverride ?? {
            completedAtMs: 1780977421069,
            item: {
              content: [{ text: 'User says:\nhello', type: 'text' }],
              id: 'msg_user',
              type: 'userMessage',
            },
            threadId: 'thread',
            turnId: 'turn',
          },
        );

  return {
    id: 10,
    runtime_id: 'runtime',
    card_id: 'card',
    wave_id: 'wave',
    thread_id: 'thread',
    turn_id: 'turn',
    item_uuid: 'msg',
    item_type: 'userMessage',
    method: 'item/completed',
    params,
    created_at_ms: 1780977420000,
    ...rowOverrides,
  };
}

function userParams(text: string) {
  return {
    completedAtMs: 1780977421069,
    item: {
      content: [{ text, type: 'text' }],
      id: 'msg_user',
      type: 'userMessage',
    },
    threadId: 'thread',
    turnId: 'turn',
  };
}

describe('parseHarnessItem', () => {
  it('strips the wave diff block before extracting User says text', () => {
    const entry = parseHarnessItem(
      harnessRow({
        params: userParams(
          [
            '## Wave state changes since your last turn',
            '',
            '- report changed',
            '',
            '---',
            '',
            'User says:\nWhat changed?',
          ].join('\n'),
        ),
      }),
    );

    expect(entry).toMatchObject({
      kind: 'user',
      text: 'What changed?',
      atMs: 1780977421069,
    });
  });

  it('extracts everything after the first User says marker', () => {
    const entry = parseHarnessItem(
      harnessRow({
        params: userParams(
          'Observation\nUser says:\nfirst\nUser says:\nsecond',
        ),
      }),
    );

    expect(entry).toMatchObject({
      kind: 'user',
      text: 'first\nUser says:\nsecond',
    });
  });

  it.each([
    ['A worker card finished a turn with notes', 'Worker turn finished'],
    ['The user edited the wave report body', 'Report edited'],
    ['A dispatched task completed successfully', 'Task completed'],
    ['A dispatched task failed with an error', 'Task failed'],
  ])('labels system framing: %s', (text, label) => {
    const entry = parseHarnessItem(
      harnessRow({
        params: userParams(text),
      }),
    );

    expect(entry).toMatchObject({
      kind: 'system',
      text,
      label,
    });
  });

  it('renders wave-goal fallback as a clamped user bubble', () => {
    const entry = parseHarnessItem(
      harnessRow({
        params: userParams('Build the wave report from the current files.'),
      }),
    );

    expect(entry).toMatchObject({
      kind: 'user',
      text: 'Build the wave report from the current files.',
      clamp: true,
    });
  });

  it('parses completed agent messages', () => {
    const entry = parseHarnessItem(
      harnessRow({
        item_type: 'agentMessage',
        params: {
          completedAtMs: 1780977421069,
          item: {
            id: 'msg_agent',
            phase: 'final_answer',
            text: '**Done**',
            type: 'agentMessage',
          },
          threadId: 'thread',
          turnId: 'turn',
        },
      }),
    );

    expect(entry).toMatchObject({
      kind: 'agent',
      text: '**Done**',
      atMs: 1780977421069,
    });
  });

  it('returns null for non-completed and unknown item types', () => {
    expect(
      parseHarnessItem(harnessRow({ method: 'item/started' })),
    ).toBeNull();
    expect(
      parseHarnessItem(harnessRow({ item_type: 'commandExecution' })),
    ).toBeNull();
  });

  it('joins multi-part user content before parsing', () => {
    const entry = parseHarnessItem(
      harnessRow({
        params: {
          completedAtMs: 1780977421069,
          item: {
            content: [
              { text: 'User says:\nHello ', type: 'text' },
              { text: 'world', type: 'text' },
            ],
            id: 'msg_user',
            type: 'userMessage',
          },
          threadId: 'thread',
          turnId: 'turn',
        },
      }),
    );

    expect(entry).toMatchObject({
      kind: 'user',
      text: 'Hello world',
    });
  });
});
