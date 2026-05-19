import { useEffect, useRef, useState } from 'react';
import { Crumbs, WaveRow } from '../ui';
import type { Cove, Route, Wave } from '../types';
import { DeleteButton } from './_shared';

// ============================================================
// CovePage — waves of one Cove, grouped by status.
// ============================================================

function Section({
  label,
  labelWarn,
  children,
}: {
  label: string;
  labelWarn?: boolean;
  children: React.ReactNode;
}) {
  return (
    <div style={{ marginBottom: 36 }}>
      <div
        className={'h-eyebrow' + (labelWarn ? ' warn' : '')}
        style={{ marginBottom: 0, paddingBottom: 8, borderBottom: '1px solid var(--hairline)' }}
      >
        {label}
      </div>
      <div className="waves">{children}</div>
    </div>
  );
}

export function CovePage({
  cove,
  waves,
  onGo,
  onCreateWave,
  onRenameCove,
  onDeleteCove,
  onDeleteWave,
}: {
  cove: Cove;
  waves: Wave[];
  onGo: (r: Route) => void;
  /** Called when the user submits the inline `+ New wave` compose bar. */
  onCreateWave?: (coveId: string, title: string) => void | Promise<void>;
  /** Called from the inline rename input on the header. */
  onRenameCove?: (coveId: string, name: string) => void | Promise<void>;
  /** Called from the × button on the header. CovePage shows its own
   *  `window.confirm`, so callers don't need to double-prompt. */
  onDeleteCove?: (coveId: string) => void | Promise<void>;
  /** Called from a per-row × on hover. Same confirm-then-delete pattern. */
  onDeleteWave?: (waveId: string) => void | Promise<void>;
}) {
  const deleteWaveWithConfirm = (w: Wave) => {
    if (!onDeleteWave) return;
    const sure = window.confirm(
      `Delete wave "${w.title}"? Its cards (including any terminals) go too. This cannot be undone.`,
    );
    if (!sure) return;
    void onDeleteWave(w.id);
  };
  const running = waves.filter((w) => w.status === 'running');
  const waiting = waves.filter((w) => w.status === 'waiting');
  const idle    = waves.filter((w) => w.status === 'idle');
  // Derived eyebrow — the kernel has no `subtitle` field, so we compose
  // one from wave counts. Empty when the cove is empty, in which case
  // the eyebrow drops to just the color chip.
  const eyebrow = (() => {
    if (waves.length === 0) return '';
    const noun = waves.length === 1 ? 'wave' : 'waves';
    if (running.length === 0) return `${waves.length} ${noun}`;
    return `${waves.length} ${noun} · ${running.length} running`;
  })();

  return (
    <div className="col wide">
      <Crumbs
        items={[
          { label: 'Today', onClick: () => onGo({ name: 'today' }) },
          { label: cove.name },
        ]}
      />
      <div
        className="h-eyebrow"
        style={{ display: 'flex', alignItems: 'center', gap: 8 }}
      >
        <span
          style={{
            width: 10, height: 10, borderRadius: 3,
            background: cove.color, display: 'inline-block',
          }}
        />
        {eyebrow}
      </div>
      <div style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
        {onRenameCove ? (
          <EditableTitle
            value={cove.name}
            ariaLabel="Cove name"
            onSave={(name) => onRenameCove(cove.id, name)}
          />
        ) : (
          <h1 className="h-display" style={{ flex: 1, margin: 0 }}>{cove.name}.</h1>
        )}
        {onDeleteCove && (
          <DeleteButton
            label={`Delete cove "${cove.name}"`}
            confirmMessage={`Delete cove "${cove.name}"? Its waves and cards go too. This cannot be undone.`}
            onDelete={() => onDeleteCove(cove.id)}
          />
        )}
      </div>

      {waves.length === 0 && (
        <div
          style={{
            padding: '32px 0 8px', color: 'var(--text-3)',
            fontSize: 15, textAlign: 'center',
          }}
        >
          This Cove is quiet. Start a Wave below.
        </div>
      )}

      {waiting.length > 0 && (
        <Section label="Waiting on you" labelWarn>
          {waiting.map((w) => (
            <WaveRow
              key={w.id}
              wave={w}
              cove={cove}
              showCove={false}
              onClick={() => onGo({ name: 'wave', id: w.id })}
              onDelete={onDeleteWave ? () => deleteWaveWithConfirm(w) : undefined}
            />
          ))}
        </Section>
      )}
      {running.length > 0 && (
        <Section label="Running">
          {running.map((w) => (
            <WaveRow
              key={w.id}
              wave={w}
              cove={cove}
              showCove={false}
              onClick={() => onGo({ name: 'wave', id: w.id })}
              onDelete={onDeleteWave ? () => deleteWaveWithConfirm(w) : undefined}
            />
          ))}
        </Section>
      )}
      {idle.length > 0 && (
        <Section label="Idle">
          {idle.map((w) => (
            <WaveRow
              key={w.id}
              wave={w}
              cove={cove}
              showCove={false}
              onClick={() => onGo({ name: 'wave', id: w.id })}
              onDelete={onDeleteWave ? () => deleteWaveWithConfirm(w) : undefined}
            />
          ))}
        </Section>
      )}

      {onCreateWave && (
        <NewWaveCTA
          onSubmit={(title) => onCreateWave(cove.id, title)}
        />
      )}
    </div>
  );
}

/**
 * Title with an inline-edit affordance.
 *
 * The pencil button switches the h1 to a same-sized input. Enter / blur
 * save (no-op if unchanged or empty); Escape cancels. The input inherits
 * the h1's visual styling so editing feels like the title sliding open,
 * not a popover. The trailing period in the design (`cove.name + '.'`)
 * is rendered by the parent, not stored — the editor edits the raw name.
 */
function EditableTitle({
  value,
  onSave,
  ariaLabel,
}: {
  value: string;
  onSave: (next: string) => void | Promise<void>;
  ariaLabel: string;
}) {
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState(value);
  const inputRef = useRef<HTMLInputElement | null>(null);

  // External value changes (e.g. WS event from another tab) should not
  // clobber an in-flight edit; only sync `draft` when not editing.
  useEffect(() => {
    if (!editing) setDraft(value);
  }, [editing, value]);

  const enter = () => {
    setDraft(value);
    setEditing(true);
    queueMicrotask(() => {
      inputRef.current?.focus();
      inputRef.current?.select();
    });
  };
  const cancel = () => setEditing(false);
  const save = async () => {
    const trimmed = draft.trim();
    setEditing(false);
    if (!trimmed || trimmed === value) return;
    await onSave(trimmed);
  };

  if (editing) {
    return (
      <input
        ref={inputRef}
        value={draft}
        onChange={(e) => setDraft(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === 'Enter') void save();
          else if (e.key === 'Escape') cancel();
        }}
        onBlur={() => void save()}
        aria-label={ariaLabel}
        className="h-display"
        style={{
          flex: 1,
          minWidth: 0,
          background: 'transparent',
          border: 'none',
          outline: 'none',
          padding: 0,
          margin: 0,
        }}
      />
    );
  }
  // Click-to-edit: no pencil affordance — the title itself is the
  // affordance. `cursor: text` is the hint; click → enter edit mode.
  return (
    <h1
      className="h-display"
      style={{ flex: 1, margin: 0, cursor: 'text' }}
      onClick={enter}
      title="Click to rename"
    >
      {value}.
    </h1>
  );
}

// ---------------- NewWaveCTA — CovePage's compose-bar ----------------
//
// Bottom-of-page ghost button by default; expands inline to a single-line
// text input on click. Visually rhymes with WavePage's `+ Add panel` so
// the "compose at the bottom" idiom is consistent across pages.

function NewWaveCTA({
  onSubmit,
}: {
  onSubmit: (title: string) => void | Promise<void>;
}) {
  const [open, setOpen] = useState(false);
  const [title, setTitle] = useState('');
  const inputRef = useRef<HTMLInputElement | null>(null);

  const openForm = () => {
    setOpen(true);
    queueMicrotask(() => inputRef.current?.focus());
  };
  const close = () => {
    setOpen(false);
    setTitle('');
  };
  const submit = async () => {
    const trimmed = title.trim();
    if (!trimmed) {
      close();
      return;
    }
    await onSubmit(trimmed);
    close();
  };

  if (!open) {
    return (
      <button
        className="add-panel"
        onClick={openForm}
        title="New wave"
        style={{ marginTop: 16 }}
      >
        + New wave
      </button>
    );
  }
  return (
    <div
      style={{
        marginTop: 16,
        padding: '10px 14px',
        border: '1px dashed var(--text-3, oklch(60% 0.005 245))',
        borderRadius: 8,
        display: 'flex',
        alignItems: 'center',
        gap: 8,
      }}
    >
      <span style={{ color: 'var(--text-3)', flexShrink: 0 }}>›</span>
      <input
        ref={inputRef}
        value={title}
        onChange={(e) => setTitle(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === 'Enter') void submit();
          else if (e.key === 'Escape') close();
        }}
        onBlur={() => void submit()}
        placeholder="Wave title…"
        style={{
          flex: 1,
          minWidth: 0,
          font: 'inherit',
          background: 'transparent',
          border: 'none',
          outline: 'none',
          color: 'var(--text)',
        }}
      />
      <span style={{ color: 'var(--text-3)', fontSize: 12 }}>↵</span>
    </div>
  );
}
