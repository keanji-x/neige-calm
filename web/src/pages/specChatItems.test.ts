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

  it('drops empty completed known message rows', () => {
    expect(
      parseHarnessItem(
        harnessRow({
          params: userParams('User says:\n   '),
        }),
      ),
    ).toBeNull();

    expect(
      parseHarnessItem(
        harnessRow({
          item_type: 'agentMessage',
          params: {
            completedAtMs: 1780977421069,
            item: {
              id: 'msg_agent',
              text: '   ',
              type: 'agentMessage',
            },
          },
        }),
      ),
    ).toBeNull();
  });

  it('returns null for non-completed rows', () => {
    expect(
      parseHarnessItem(harnessRow({ method: 'item/started' })),
    ).toBeNull();
  });

  it('parses completed command executions', () => {
    const entry = parseHarnessItem(
      harnessRow({
        item_type: 'commandExecution',
        params: {
          completedAtMs: 1780977421069,
          item: {
            id: 'cmd_1',
            type: 'commandExecution',
            command: 'npm test',
            aggregatedOutput: 'ok',
            exitCode: 0,
            durationMs: 1234,
          },
        },
      }),
    );

    expect(entry).toMatchObject({
      kind: 'run',
      command: 'npm test',
      output: 'ok',
      exitCode: 0,
      durationMs: 1234,
      atMs: 1780977421069,
    });
  });

  it('parses errored MCP tool calls', () => {
    const entry = parseHarnessItem(
      harnessRow({
        item_type: 'mcpToolCall',
        params: {
          completedAtMs: 1780977421069,
          item: {
            id: 'tool_1',
            type: 'mcpToolCall',
            server: 'filesystem',
            tool: 'read_file',
            arguments: { path: 'src/app.ts' },
            error: { message: 'denied' },
            durationMs: 42,
          },
        },
      }),
    );

    expect(entry).toMatchObject({
      kind: 'tool',
      server: 'filesystem',
      tool: 'read_file',
      args: JSON.stringify({ path: 'src/app.ts' }, null, 2),
      result: JSON.stringify({ message: 'denied' }, null, 2),
      isError: true,
      durationMs: 42,
    });
  });

  it('parses multi-file changes', () => {
    const entry = parseHarnessItem(
      harnessRow({
        item_type: 'fileChange',
        params: {
          completedAtMs: 1780977421069,
          item: {
            id: 'edit_1',
            type: 'fileChange',
            status: 'declined',
            changes: [
              {
                path: 'src/a.ts',
                diff: '--- a\n+++ b',
                kind: { type: 'update' },
              },
              {
                path: 'src/b.ts',
                diff: 'new file',
                kind: { type: 'add' },
              },
            ],
          },
        },
      }),
    );

    expect(entry).toMatchObject({
      kind: 'edit',
      status: 'declined',
      changes: [
        { path: 'src/a.ts', diff: '--- a\n+++ b', verb: 'update' },
        { path: 'src/b.ts', diff: 'new file', verb: 'add' },
      ],
    });
  });

  it('drops empty reasoning rows', () => {
    const entry = parseHarnessItem(
      harnessRow({
        item_type: 'reasoning',
        params: {
          completedAtMs: 1780977421069,
          item: {
            id: 'reason_1',
            type: 'reasoning',
            summary: [],
            content: [],
          },
        },
      }),
    );

    expect(entry).toBeNull();
  });

  it('parses populated reasoning rows', () => {
    const entry = parseHarnessItem(
      harnessRow({
        item_type: 'reasoning',
        params: {
          completedAtMs: 1780977421069,
          item: {
            id: 'reason_1',
            type: 'reasoning',
            summary: ['Thinking about X'],
            content: ['detail Y'],
          },
        },
      }),
    );

    expect(entry).toMatchObject({
      kind: 'reasoning',
      summary: 'Thinking about X',
      detail: 'detail Y',
    });
  });

  it('parses context compaction rows', () => {
    const entry = parseHarnessItem(
      harnessRow({
        item_type: 'contextCompaction',
        params: {
          completedAtMs: 1780977421069,
          item: {
            id: 'compact_1',
            type: 'contextCompaction',
          },
        },
      }),
    );

    expect(entry).toMatchObject({
      kind: 'compact',
      atMs: 1780977421069,
    });
  });

  it('falls back for unknown completed item types', () => {
    const entry = parseHarnessItem(
      harnessRow({
        item_type: null,
        params: {
          completedAtMs: 1780977421069,
          item: {
            id: 'legacy_1',
            type: 'legacyThing',
          },
        },
      }),
    );

    expect(entry).toMatchObject({
      kind: 'unknown',
      itemType: 'legacyThing',
    });
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
