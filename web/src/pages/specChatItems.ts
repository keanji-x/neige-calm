import type { HarnessItem } from '../api/generated-events';

type ChatEntryBase = {
  id: number;
  atMs: number;
};

type UserChatEntry = ChatEntryBase & {
  kind: 'user';
  text: string;
  label?: string;
  clamp?: boolean;
};

type AgentChatEntry = ChatEntryBase & {
  kind: 'agent';
  text: string;
  label?: string;
  clamp?: boolean;
};

type SystemChatEntry = ChatEntryBase & {
  kind: 'system';
  text: string;
  label?: string;
  clamp?: boolean;
};

export type ChatEntry =
  | UserChatEntry
  | AgentChatEntry
  | SystemChatEntry
  | (ChatEntryBase & {
      kind: 'run';
      command: string;
      output: string;
      exitCode: number | null;
      durationMs: number | null;
    })
  | (ChatEntryBase & {
      kind: 'tool';
      server: string;
      tool: string;
      args: string;
      result: string;
      isError: boolean;
      durationMs: number | null;
    })
  | (ChatEntryBase & {
      kind: 'reasoning';
      summary: string;
      detail: string;
    })
  | (ChatEntryBase & {
      kind: 'edit';
      status: string;
      changes: Array<{ path: string; diff: string; verb: string }>;
    })
  | (ChatEntryBase & {
      kind: 'compact';
    })
  | (ChatEntryBase & {
      kind: 'unknown';
      itemType: string;
    });

type ParsedParams = {
  completedAtMs?: unknown;
  item?: unknown;
};

type TextContentPart = {
  type?: unknown;
  text?: unknown;
};

const DIFF_BLOCK_PREFIX = '## Wave state changes since your last turn';
const DIFF_BLOCK_END = '\n\n---\n\n';
const USER_SAYS_MARKER = 'User says:\n';

const SYSTEM_LABELS: Array<[prefix: string, label: string]> = [
  ['A worker card finished a turn', 'Worker turn finished'],
  ['The user edited the wave report', 'Report edited'],
  ['A dispatched task completed', 'Task completed'],
  ['A dispatched task failed', 'Task failed'],
];

function parseParams(params: string): ParsedParams | null {
  try {
    const parsed = JSON.parse(params) as unknown;
    if (parsed === null || typeof parsed !== 'object') return null;
    return parsed as ParsedParams;
  } catch {
    return null;
  }
}

function completedAtMs(row: HarnessItem, params: ParsedParams): number {
  return typeof params.completedAtMs === 'number' &&
    Number.isFinite(params.completedAtMs)
    ? params.completedAtMs
    : row.created_at_ms;
}

function itemObject(params: ParsedParams): Record<string, unknown> | null {
  if (params.item === null || typeof params.item !== 'object') return null;
  return params.item as Record<string, unknown>;
}

function stringField(value: unknown): string {
  return typeof value === 'string' ? value : '';
}

function numberField(value: unknown): number | null {
  return typeof value === 'number' && Number.isFinite(value) ? value : null;
}

function displayJson(value: unknown): string {
  if (typeof value === 'string') return value;
  if (value == null) return '';
  try {
    return JSON.stringify(value, null, 2);
  } catch {
    return String(value);
  }
}

function joinedContent(value: unknown): string {
  if (!Array.isArray(value)) return '';
  return value
    .map((part) => {
      if (typeof part === 'string') return part;
      if (part !== null && typeof part === 'object') {
        const text = (part as { text?: unknown }).text;
        return typeof text === 'string' ? text : '';
      }
      return '';
    })
    .join('');
}

function stripDiffBlockPrefix(text: string): string {
  if (!text.startsWith(DIFF_BLOCK_PREFIX)) return text;
  const endIndex = text.indexOf(DIFF_BLOCK_END);
  if (endIndex < 0) return text;
  return text.slice(endIndex + DIFF_BLOCK_END.length);
}

function userMessageText(item: Record<string, unknown>): string {
  const content = item.content;
  if (!Array.isArray(content)) return '';

  return content
    .map((part: TextContentPart) =>
      typeof part?.text === 'string' ? part.text : '',
    )
    .join('');
}

function parseAgentMessage(
  row: HarnessItem,
  params: ParsedParams,
): ChatEntry | null {
  const item = itemObject(params);
  const text = typeof item?.text === 'string' ? item.text.trim() : '';
  if (!text) return null;

  return {
    id: row.id,
    kind: 'agent',
    text,
    atMs: completedAtMs(row, params),
  };
}

function parseUserMessage(
  row: HarnessItem,
  params: ParsedParams,
): ChatEntry | null {
  const item = itemObject(params);
  if (!item) return null;

  const stripped = stripDiffBlockPrefix(userMessageText(item)).trimStart();
  const markerIndex = stripped.indexOf(USER_SAYS_MARKER);
  if (markerIndex >= 0) {
    // Batched turns can append system observations after the user text; v1
    // intentionally keeps everything after the first marker as the bubble.
    const text = stripped.slice(markerIndex + USER_SAYS_MARKER.length).trim();
    if (!text) return null;
    return {
      id: row.id,
      kind: 'user',
      text,
      atMs: completedAtMs(row, params),
    };
  }

  const text = stripped.trim();
  if (!text) return null;

  for (const [prefix, label] of SYSTEM_LABELS) {
    if (text.startsWith(prefix)) {
      return {
        id: row.id,
        kind: 'system',
        text,
        label,
        atMs: completedAtMs(row, params),
      };
    }
  }

  return {
    id: row.id,
    kind: 'user',
    text,
    atMs: completedAtMs(row, params),
    clamp: true,
  };
}

function parseCommandExecution(
  row: HarnessItem,
  params: ParsedParams,
): ChatEntry {
  const item = itemObject(params);
  return {
    id: row.id,
    kind: 'run',
    command: stringField(item?.command),
    output: stringField(item?.aggregatedOutput),
    exitCode: numberField(item?.exitCode),
    durationMs: numberField(item?.durationMs),
    atMs: completedAtMs(row, params),
  };
}

function parseMcpToolCall(row: HarnessItem, params: ParsedParams): ChatEntry {
  const item = itemObject(params);
  const error = item?.error;
  return {
    id: row.id,
    kind: 'tool',
    server: stringField(item?.server),
    tool: stringField(item?.tool),
    args: displayJson(item?.arguments),
    result: displayJson(error ?? item?.result),
    isError: error != null,
    durationMs: numberField(item?.durationMs),
    atMs: completedAtMs(row, params),
  };
}

function parseReasoning(row: HarnessItem, params: ParsedParams): ChatEntry | null {
  const item = itemObject(params);
  const summary = joinedContent(item?.summary);
  const detail = joinedContent(item?.content);
  if (summary.trim() === '' && detail.trim() === '') return null;

  return {
    id: row.id,
    kind: 'reasoning',
    summary,
    detail,
    atMs: completedAtMs(row, params),
  };
}

function parseFileChange(row: HarnessItem, params: ParsedParams): ChatEntry {
  const item = itemObject(params);
  const changes = Array.isArray(item?.changes) ? item.changes : [];
  return {
    id: row.id,
    kind: 'edit',
    status: stringField(item?.status),
    changes: changes.map((change) => {
      const record =
        change !== null && typeof change === 'object'
          ? (change as Record<string, unknown>)
          : {};
      const kind = record.kind;
      const kindRecord =
        kind !== null && typeof kind === 'object'
          ? (kind as Record<string, unknown>)
          : {};
      return {
        path: stringField(record.path),
        diff: stringField(record.diff),
        verb: stringField(kindRecord.type),
      };
    }),
    atMs: completedAtMs(row, params),
  };
}

function parseContextCompaction(
  row: HarnessItem,
  params: ParsedParams,
): ChatEntry {
  return {
    id: row.id,
    kind: 'compact',
    atMs: completedAtMs(row, params),
  };
}

function parseUnknownItem(
  row: HarnessItem,
  params: ParsedParams | null,
): ChatEntry {
  const item = params ? itemObject(params) : null;
  const itemType = (row.item_type ?? stringField(item?.type)) || 'unknown';
  return {
    id: row.id,
    kind: 'unknown',
    itemType,
    atMs: params ? completedAtMs(row, params) : row.created_at_ms,
  };
}

export function parseHarnessItem(row: HarnessItem): ChatEntry | null {
  if (row.method !== 'item/completed') return null;

  const params = parseParams(row.params);
  if (!params) return parseUnknownItem(row, null);

  switch (row.item_type) {
    case 'userMessage':
      return parseUserMessage(row, params);
    case 'agentMessage':
      return parseAgentMessage(row, params);
    case 'commandExecution':
      return parseCommandExecution(row, params);
    case 'mcpToolCall':
      return parseMcpToolCall(row, params);
    case 'fileChange':
      return parseFileChange(row, params);
    case 'reasoning':
      return parseReasoning(row, params);
    case 'contextCompaction':
      return parseContextCompaction(row, params);
    default:
      return parseUnknownItem(row, params);
  }
}
