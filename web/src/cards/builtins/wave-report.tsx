// Wave Report card — issue #229.
//
// One per wave; rendered at the top of the WaveGrid (the kernel seeds
// a layout overlay positioning it at (0, 0, 12, 4) on wave-create, and
// the migration backfill does the same for legacy waves). The payload
// is a single Markdown body the spec agent maintains via the
// `calm.report.read` / `.write` / `.edit` MCP tools (mirrors codex's
// native Read / Edit / Write file-tool surface 1:1).
//
// Section model: the card derives sections by splitting the body at H1
// (`^# `) headings. Each section is rendered with a small disclosure
// triangle so the user can collapse the noisy ones. The Timeline
// section is collapsed by default; everything else is open. Sections
// titled "Needs attention" / "Blockers" / "Attention" get a warning
// border so they catch the eye.
//
// Markdown rendering: `react-markdown` + `remark-gfm` handle the body
// (paragraphs, lists, tables, task lists, strikethrough, autolinks,
// fenced code, links, bold/italic). We deliberately do NOT wire in
// `rehype-raw` — agent-authored content must not be able to inject
// arbitrary HTML; react-markdown sanitizes by default.
//
// The lifecycle badge in the header is fed from `WaveContext`
// (`web/src/shared/components/WaveContext.ts`) — the Wave page wraps
// its children in a provider so we don't have to thread the wave
// row through every card. If the context is missing (e.g. unit tests
// that render the card in isolation) the badge silently omits.

import { useContext } from 'react';
import ReactMarkdown from 'react-markdown';
import remarkGfm from 'remark-gfm';
import { useState } from '../../shared/state';
import { z } from 'zod';
import { CardHead } from '../CardHead';
import { WaveContext } from '../../shared/components/WaveContext';
import { WaveLifecycleBadge } from '../../shared/components/WaveLifecycleBadge';
import { updateWaveReport, CalmApiError } from '../../api/calm';
import {
  WAVE_REPORT_PAYLOAD_SCHEMA_VERSION,
  payloadSchemaVersion,
} from './schemaVersions';
import type { CardEntry } from '../registry';

declare module '../../types' {
  interface WaveCardDataMap {
    'wave-report': WaveReportCardData;
  }
}

export interface WaveReportCardData {
  type: 'wave-report';
  id?: string;
  summary: string;
  body: string;
  unsupportedVersion?: number;
}

/** Strict zod schema for the wire payload. `schemaVersion` may be
 *  absent (treated as v1 — historical rows pre-PR B don't exist in
 *  practice but we keep the absent-as-1 contract uniform). Future v2
 *  fields would be added as optional. */
export const waveReportPayloadSchema = z.object({
  schemaVersion: z.number().int().optional(),
  summary: z.string(),
  body: z.string(),
});

interface ParsedSection {
  /** Raw heading text (without the leading `# `). */
  title: string;
  /** Slug derived from `title` — used as the localStorage key suffix
   *  + the section's `id` for in-page anchors. */
  slug: string;
  /** Body after the H1, before the next H1 (no trailing newline trim
   *  so embedded code blocks keep their shape). */
  body: string;
}

const ATTENTION_HEADING_RE = /^(needs attention|blockers|attention)$/i;
const TIMELINE_SLUG = 'timeline';

/** Stable slug from a title: lowercase, replace runs of non-alnum
 *  with `-`, trim leading/trailing dashes. */
function slugify(s: string): string {
  return s
    .toLowerCase()
    .normalize('NFKD')
    .replace(/[^a-z0-9]+/g, '-')
    .replace(/^-+|-+$/g, '');
}

/**
 * Split a Markdown body into H1-anchored sections. The text *before*
 * the first H1 (if any) is treated as a leading "Preamble" section
 * with slug `_preamble`. This gives the card a stable section list
 * even when the agent leaves the canonical headings out (a partial
 * agent writeup still renders coherently).
 */
export function parseSections(body: string): ParsedSection[] {
  const lines = body.split('\n');
  const sections: ParsedSection[] = [];
  // Index of the current section's heading line; -1 means we're in
  // the leading preamble before the first heading.
  let cursorHeadingIdx = -1;
  let buf: string[] = [];

  const flush = (heading: string | null) => {
    const body = buf.join('\n');
    if (heading === null) {
      // Preamble: only emit if it actually has non-blank content.
      if (body.trim().length > 0) {
        sections.push({ title: 'Preamble', slug: '_preamble', body });
      }
    } else {
      const title = heading.replace(/^#\s+/, '').trim();
      const slug = slugify(title) || '_section';
      sections.push({ title, slug, body });
    }
    buf = [];
  };

  for (let i = 0; i < lines.length; i++) {
    const line = lines[i];
    if (/^#\s+/.test(line)) {
      // Flush whatever we were collecting, then start a new section.
      if (cursorHeadingIdx === -1) {
        flush(null);
      } else {
        flush(lines[cursorHeadingIdx]);
      }
      cursorHeadingIdx = i;
    } else {
      buf.push(line);
    }
  }
  // Trailing section.
  if (cursorHeadingIdx === -1) {
    flush(null);
  } else {
    flush(lines[cursorHeadingIdx]);
  }
  return sections;
}

function isAttentionSection(title: string): boolean {
  return ATTENTION_HEADING_RE.test(title.trim());
}

function defaultCollapsed(slug: string): boolean {
  return slug === TIMELINE_SLUG;
}

function localStorageKey(waveId: string, slug: string): string {
  return `wave-report:${waveId}:section:${slug}:collapsed`;
}

function readPersistedCollapsed(waveId: string | null, slug: string): boolean | null {
  if (!waveId) return null;
  try {
    const v = window.localStorage.getItem(localStorageKey(waveId, slug));
    if (v === '1') return true;
    if (v === '0') return false;
    return null;
  } catch {
    // Storage disabled / sandboxed test env: silently fall back to default.
    return null;
  }
}

function writePersistedCollapsed(waveId: string | null, slug: string, collapsed: boolean): void {
  if (!waveId) return;
  try {
    window.localStorage.setItem(localStorageKey(waveId, slug), collapsed ? '1' : '0');
  } catch {
    // ignore — non-fatal.
  }
}

/** Lazy initializer: resolve initial collapsed state from
 *  localStorage if present, else apply the per-slug default. */
function resolveInitialCollapsed(waveId: string | null, sections: ParsedSection[]): Record<string, boolean> {
  const out: Record<string, boolean> = {};
  for (const s of sections) {
    const persisted = readPersistedCollapsed(waveId, s.slug);
    out[s.slug] = persisted !== null ? persisted : defaultCollapsed(s.slug);
  }
  return out;
}

interface ReportSectionProps {
  section: ParsedSection;
  collapsed: boolean;
  onToggle: () => void;
}

function ReportSection({ section, collapsed, onToggle }: ReportSectionProps) {
  const attention = isAttentionSection(section.title);
  const className = `wave-report-section${attention ? ' attention' : ''}`;
  return (
    <section className={className} id={`wave-report-section-${section.slug}`}>
      <button
        type="button"
        className="wave-report-section-toggle"
        aria-expanded={!collapsed}
        aria-controls={`wave-report-section-body-${section.slug}`}
        onClick={onToggle}
      >
        <span aria-hidden="true" className="wave-report-section-caret">
          {collapsed ? '▸' : '▾'}
        </span>
        <span className="wave-report-section-title">{section.title}</span>
      </button>
      {!collapsed && (
        <div className="wave-report-section-body" id={`wave-report-section-body-${section.slug}`}>
          <ReactMarkdown remarkPlugins={[remarkGfm]}>{section.body}</ReactMarkdown>
        </div>
      )}
    </section>
  );
}

function UnsupportedWaveReportCard({
  version,
  onClose,
}: {
  version: number;
  onClose?: () => void;
}) {
  return (
    <div className="wave-report-card wave-report-card-unsupported-version">
      <CardHead
        card={{ type: 'wave-report', summary: '', body: '' }}
        className="card-drag-handle"
        title="Report"
        onClose={onClose}
        closeAriaLabel="Remove panel"
      />
      <div className="wave-report-empty">
        Unsupported card payload version (got {version}, frontend supports{' '}
        {WAVE_REPORT_PAYLOAD_SCHEMA_VERSION}); please refresh.
      </div>
    </div>
  );
}

function WaveReportCard({
  card,
  onClose,
}: {
  card: WaveReportCardData;
  onClose?: () => void;
}) {
  if (card.unsupportedVersion !== undefined) {
    return (
      <UnsupportedWaveReportCard version={card.unsupportedVersion} onClose={onClose} />
    );
  }
  return <WaveReportCardImpl card={card} onClose={onClose} />;
}

/** Inline pencil icon for the "edit report" affordance. Matches the
 *  `CloseIcon` convention: inline SVG, currentColor stroke, 1em-sized
 *  via CSS so the parent button's `font-size` controls the glyph. */
function EditIcon() {
  return (
    <svg
      width="1em"
      height="1em"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.5"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
      focusable="false"
    >
      {/* Classic pencil glyph: barrel + tip + nib. */}
      <path d="M4 20 L4.5 16 L16.5 4 L20 7.5 L8 19.5 L4 20 Z" />
      <path d="M14 6 L18 10" />
    </svg>
  );
}

/** Map a failed save call to a Chinese-language error message the user
 *  can act on. Server `code` strings are stable (see `ErrorBody`
 *  generated.ts); falling back to the human-readable message keeps the
 *  UX informative for any code we haven't enumerated yet. */
function formatSaveError(err: unknown): string {
  if (err instanceof CalmApiError) {
    if (err.status === 401) return '请先登录';
    if (err.status === 403) return '无权编辑此报告';
    if (err.status === 400 || err.status === 422) {
      return err.message?.length > 0 ? err.message : '提交格式不对';
    }
    if (err.status >= 500) return '保存失败，请重试';
    return err.message?.length > 0 ? err.message : '保存失败，请重试';
  }
  return '保存失败，请重试';
}

interface EditState {
  body: string;
  submitting: boolean;
  error: string | null;
}

interface ReadOnlyViewProps {
  summary: string;
  body: string;
  waveId: string | null;
}

function ReadOnlyView({ summary, body, waveId }: ReadOnlyViewProps) {
  const sections = parseSections(body);
  // Per-section collapse state, keyed by slug. Lazy-init so
  // localStorage reads happen once per mount.
  const [collapsedBySlug, setCollapsedBySlug] = useState<Record<string, boolean>>(
    () => resolveInitialCollapsed(waveId, sections),
  );

  const toggle = (slug: string) => {
    setCollapsedBySlug((prev) => {
      const next = !(prev[slug] ?? defaultCollapsed(slug));
      writePersistedCollapsed(waveId, slug, next);
      return { ...prev, [slug]: next };
    });
  };

  const summaryLine = summary.trim();
  return (
    <>
      {summaryLine.length > 0 && (
        <div className="wave-report-summary" aria-label="Wave report summary">
          {summaryLine}
        </div>
      )}
      <div className="wave-report-body">
        {sections.length === 0 ? (
          <div className="wave-report-empty">
            <em>Spec agent has not produced a report yet.</em>
          </div>
        ) : (
          sections.map((s) => (
            <ReportSection
              key={s.slug}
              section={s}
              collapsed={collapsedBySlug[s.slug] ?? defaultCollapsed(s.slug)}
              onToggle={() => toggle(s.slug)}
            />
          ))
        )}
      </div>
    </>
  );
}

interface EditViewProps {
  /** Current summary, passed through unchanged on save. The edit UI
   *  intentionally does NOT surface this as a field: `summary` is an
   *  AI-maintained derivative (the spec agent regenerates it via
   *  `calm.report.write` once it sees the new body), so asking the
   *  user to keep it in sync is unnecessary cognitive load. We still
   *  send it on the wire to satisfy the server's `deny_unknown_fields`
   *  payload shape — passing `summary` unchanged is a no-op for the
   *  read-mode rendering. */
  summary: string;
  initialBody: string;
  onSave: (
    summary: string,
    body: string,
  ) => Promise<{ summary: string; body: string }>;
  onCancel: () => void;
  onSaved: (next: { summary: string; body: string }) => void;
}

function EditView({
  summary,
  initialBody,
  onSave,
  onCancel,
  onSaved,
}: EditViewProps) {
  const [state, setState] = useState<EditState>({
    body: initialBody,
    submitting: false,
    error: null,
  });

  const submit = async () => {
    setState((s) => ({ ...s, submitting: true, error: null }));
    try {
      // Send `summary` through unchanged — the AI repopulates it on
      // its next `report.write` based on the new body. See EditViewProps.
      const next = await onSave(summary, state.body);
      // Hand the freshly-projected payload to the parent so the
      // post-merge text replaces the local edits (the kernel may
      // have normalised the body, and we want the user to see the
      // committed state, not their typed copy).
      onSaved(next);
    } catch (err) {
      setState((s) => ({
        ...s,
        submitting: false,
        error: formatSaveError(err),
      }));
    }
  };

  return (
    <div className="wave-report-edit">
      <textarea
        id="wave-report-edit-body"
        className="wave-report-edit-body"
        value={state.body}
        onChange={(e) => setState((s) => ({ ...s, body: e.target.value }))}
        placeholder={`# Goal\n\n...`}
        rows={20}
        disabled={state.submitting}
        aria-label="Wave report body"
        // Don't let RGL pick this up as a drag start — the textarea
        // sits outside the `.card-drag-handle` header but defensive
        // stopPropagation costs nothing and protects future layouts.
        onMouseDown={(e) => e.stopPropagation()}
      />
      {state.error && (
        <p className="wave-report-edit-error" role="alert">
          {state.error}
        </p>
      )}
      <div className="wave-report-edit-actions">
        <button
          type="button"
          className="wave-report-edit-cancel"
          onClick={onCancel}
          disabled={state.submitting}
        >
          Cancel
        </button>
        <button
          type="button"
          className="wave-report-edit-save"
          onClick={() => {
            void submit();
          }}
          disabled={state.submitting}
          aria-disabled={state.submitting}
        >
          {state.submitting ? '保存中…' : 'Save'}
        </button>
      </div>
    </div>
  );
}

function WaveReportCardImpl({
  card,
  onClose,
}: {
  card: WaveReportCardData;
  onClose?: () => void;
}) {
  const waveCtx = useContext(WaveContext);
  const waveId = waveCtx?.id ?? null;

  // Optimistic display state. Seeded from props; after a successful
  // user edit we adopt the projected payload immediately so the user
  // doesn't have to wait for the `card.updated` event to roll back
  // through the WS bus. The parent re-render will overwrite via the
  // `key` discriminator below if it diverges.
  const [editing, setEditing] = useState(false);
  const [override, setOverride] = useState<{
    summary: string;
    body: string;
  } | null>(null);

  // Prefer the locally-applied save projection over the prop. If the
  // prop later catches up (server-pushed event) we'd ideally drop
  // `override`, but a stale optimistic-override is harmless: it
  // matches what the user just submitted, and the next edit clears
  // `override` to a fresher value.
  const summary = override ? override.summary : card.summary;
  const body = override ? override.body : card.body;

  const canEdit = waveId !== null;

  return (
    <div className="wave-report-card">
      <CardHead
        card={card}
        className="card-drag-handle"
        title="Report"
        onClose={onClose}
        closeAriaLabel="Remove panel"
        status={
          waveCtx ? <WaveLifecycleBadge lifecycle={waveCtx.lifecycle} /> : undefined
        }
      >
        {/* Edit pencil — sits between the title slot and the status
            badge so the close button (absolutely positioned by
            CardHead) doesn't overlap it. Hidden when we have no
            wave id (defensive — the headless-test renders without
            WaveContext, and there's no wave to POST against). */}
        {canEdit && !editing && (
          <button
            type="button"
            className="wave-report-edit-button"
            aria-label="Edit report"
            title="Edit report"
            onClick={(e) => {
              e.stopPropagation();
              setEditing(true);
            }}
            onMouseDown={(e) => e.stopPropagation()}
          >
            <EditIcon />
          </button>
        )}
      </CardHead>
      {editing ? (
        <EditView
          summary={summary}
          initialBody={body}
          onSave={async (summaryIn, bodyIn) => {
            // `canEdit` gates rendering of the trigger, but TS doesn't
            // narrow from a render-time check — assert here.
            if (waveId === null) {
              throw new Error('wave id missing');
            }
            const next = await updateWaveReport(waveId, {
              summary: summaryIn,
              body: bodyIn,
            });
            return { summary: next.summary, body: next.body };
          }}
          onCancel={() => setEditing(false)}
          onSaved={(next) => {
            setOverride({ summary: next.summary, body: next.body });
            setEditing(false);
          }}
        />
      ) : (
        <ReadOnlyView summary={summary} body={body} waveId={waveId} />
      )}
    </div>
  );
}

export const WaveReportEntry: CardEntry<WaveReportCardData> = {
  type: 'wave-report',
  Component: WaveReportCard,
  // Right half of the wave, full height — matches the kernel's
  // seeded layout overlay (`{x: 6, y: 0, w: 6, h: 12}`) which
  // pairs with the spec agent card on the left. Min width keeps
  // section headings readable; min height keeps at least one
  // section visible.
  defaultSize: { w: 6, h: 12, minW: 4, minH: 6 },
  claim: { mode: 'exact', kind: 'wave-report' },
  title: () => 'Report',
  accessibleName: (card) =>
    card.summary.trim().length > 0 ? `Report: ${card.summary}` : 'Report',
  create: { mode: 'kernel-minted-only' },
  fromKernel: (k) => {
    if (k.kind !== 'wave-report') return null;
    const candidate = k.payload ?? {};
    const version = payloadSchemaVersion(candidate);
    if (version > WAVE_REPORT_PAYLOAD_SCHEMA_VERSION) {
      // eslint-disable-next-line no-console
      console.warn(
        `[cards] wave-report payload schemaVersion=${version} unsupported (frontend supports ${WAVE_REPORT_PAYLOAD_SCHEMA_VERSION}); please refresh`,
        { id: k.id },
      );
      return {
        type: 'wave-report',
        id: k.id,
        summary: '',
        body: '',
        unsupportedVersion: version,
      };
    }
    const parsed = waveReportPayloadSchema.safeParse(candidate);
    if (!parsed.success) {
      // eslint-disable-next-line no-console
      console.warn(
        `[cards] wave-report payload invalid for ${k.id}:`,
        parsed.error.issues,
      );
      return null;
    }
    return {
      type: 'wave-report',
      id: k.id,
      summary: parsed.data.summary,
      body: parsed.data.body,
    };
  },
  // NO addPanel — wave-report cards are kernel-minted. The user
  // cannot add another one via the AddPanel menu.
};
