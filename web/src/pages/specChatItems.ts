import type { HarnessItem } from '../api/generated-events';

export type ChatEntry = {
  id: number;
  kind: 'user' | 'agent' | 'system';
  text: string;
  label?: string;
  atMs: number;
  clamp?: boolean;
};

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

export function parseHarnessItem(row: HarnessItem): ChatEntry | null {
  if (row.method !== 'item/completed') return null;
  if (row.item_type !== 'userMessage' && row.item_type !== 'agentMessage') {
    return null;
  }

  const params = parseParams(row.params);
  if (!params) return null;

  return row.item_type === 'agentMessage'
    ? parseAgentMessage(row, params)
    : parseUserMessage(row, params);
}
