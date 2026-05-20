// Codex (OpenAI) agent card.
//
// The kernel side spawns codex CLI bound to this card; hook events
// (PreToolUse / PostToolUse / Stop / ...) stream over the WS event bus
// on `card:<card_id>` as `codex.hook` envelopes. This component
// subscribes to its own card id and renders the events in reverse-chrono
// order. MVP: no fancy formatting — the goal here is verifying the
// end-to-end path works, not polished UI.

import { useEffect, useRef, useState } from 'react';
import { z } from 'zod';
import type { CodexCardData } from '../../types';
import { sharedEventStream } from '../../api/events';
import type { CardEntry } from '../registry';

const codexPayloadSchema = z.object({
  initial_prompt: z.string().optional(),
  model: z.string().optional(),
  cwd: z.string().optional(),
});

interface HookRow {
  /** Monotonic insertion id — used as React key. */
  seq: number;
  /** Server-derived discriminator: `hook.codex.<event>` */
  kind: string;
  /** `hook_event_name` raw, e.g. `PreToolUse`. */
  eventName: string;
  /** First-pass short summary (tool name, command preview). */
  summary: string;
  /** Wallclock receive time. */
  at: number;
}

function CodexCard({ card }: { card: CodexCardData }) {
  const cardId = card.id;
  const [rows, setRows] = useState<HookRow[]>([]);
  const seqRef = useRef(0);

  useEffect(() => {
    if (!cardId) return;
    const stream = sharedEventStream();
    stream.addTopic(`card:${cardId}`);
    const off = stream.on((ev) => {
      if (ev.ev !== 'codex.hook') return;
      if (ev.data.card_id !== cardId) return;
      const payload = (ev.data.payload ?? {}) as Record<string, unknown>;
      const eventName = String(payload.hook_event_name ?? 'unknown');
      const toolName = typeof payload.tool_name === 'string' ? payload.tool_name : '';
      const summary = summarizeHook(payload, toolName);
      const next: HookRow = {
        seq: ++seqRef.current,
        kind: ev.data.kind,
        eventName,
        summary,
        at: Date.now(),
      };
      // Newest first — prepend.
      setRows((cur) => [next, ...cur].slice(0, 200));
    });
    return () => {
      off();
      // Topic intentionally NOT removed: another tab / re-mount of the
      // same card would otherwise lose its subscription mid-stream. The
      // topic set is sticky on the shared stream by design.
    };
  }, [cardId]);

  return (
    <div className="codex-card">
      <div className="codex-card-head card-drag-handle">
        <span className="codex-card-title">Codex</span>
        {card.model && <span className="codex-card-model">{card.model}</span>}
      </div>
      <div className="codex-card-prompt">
        <span className="codex-card-prompt-label">Prompt:</span>
        <span className="codex-card-prompt-text">{card.initialPrompt || '(none)'}</span>
      </div>
      <ol className="codex-card-hooks">
        {rows.length === 0 ? (
          <li className="codex-card-empty">
            Waiting for hook events… (PreToolUse / PostToolUse / Stop)
          </li>
        ) : (
          rows.map((r) => (
            <li key={r.seq} className="codex-card-hook">
              <span className={`codex-card-hook-name evt-${r.kind.replaceAll('.', '-')}`}>
                {r.eventName}
              </span>
              <span className="codex-card-hook-summary">{r.summary}</span>
            </li>
          ))
        )}
      </ol>
    </div>
  );
}

function summarizeHook(payload: Record<string, unknown>, toolName: string): string {
  // Cheap, no JSON-pretty-print — we just want a one-liner so the user
  // can see *which* tool ran. Full payload inspection is a later UX pass.
  if (toolName) {
    const input = payload.tool_input;
    if (input && typeof input === 'object') {
      const cmd = (input as Record<string, unknown>).command;
      if (typeof cmd === 'string') {
        return `${toolName} — ${truncate(cmd, 80)}`;
      }
      const path = (input as Record<string, unknown>).path;
      if (typeof path === 'string') {
        return `${toolName} — ${path}`;
      }
    }
    return toolName;
  }
  const prompt = payload.user_prompt;
  if (typeof prompt === 'string') return truncate(prompt, 100);
  return '';
}

function truncate(s: string, n: number): string {
  return s.length > n ? s.slice(0, n - 1) + '…' : s;
}

export const CodexEntry: CardEntry<CodexCardData> = {
  type: 'codex',
  Component: CodexCard,
  defaultSize: { w: 6, h: 10, minW: 4, minH: 6 },
  fromKernel: (k) => {
    if (k.kind !== 'codex') return null;
    const candidate = k.payload ?? {};
    const parsed = codexPayloadSchema.safeParse(candidate);
    if (!parsed.success) {
      // eslint-disable-next-line no-console
      console.warn(`[cards] codex payload invalid for ${k.id}:`, parsed.error.issues);
      return null;
    }
    return {
      type: 'codex',
      id: k.id,
      initialPrompt: parsed.data.initial_prompt ?? '',
      model: parsed.data.model,
      cwd: parsed.data.cwd,
    };
  },
  addPanel: {
    label: 'New codex',
    icon: 'spark',
    createSchema: {
      fields: [
        {
          key: 'initial_prompt',
          label: 'Initial prompt',
          type: 'textarea',
          required: true,
          placeholder: 'What should codex do?',
        },
        {
          key: 'model',
          label: 'Model',
          type: 'enum',
          options: ['', 'gpt-5.4', 'o4-mini'],
          default: '',
        },
        {
          key: 'cwd',
          label: 'Working directory',
          type: 'string',
          placeholder: '$HOME',
        },
        {
          key: 'permission_mode',
          label: 'Permission mode',
          type: 'enum',
          options: ['default', 'acceptEdits', 'plan', 'dontAsk', 'bypassPermissions'],
          default: 'default',
        },
      ],
    },
  },
};
