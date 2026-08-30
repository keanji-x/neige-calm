// The `task` block — a unit of work the spec agent declared inside the report.
//
// **It is set in the document's own voice.** It used to carry five typographic
// registers in one small box — an uppercase tracked `TASK`, a mono key, an
// uppercase `CODEX`, a sans body, mono commands, a label column — while the
// prose around it had one. Worse, uppercase-plus-tracking is this system's
// *chrome* vocabulary (§2.2 reserves it for panel labels), so the block read
// as a piece of UI that had fallen into an essay.
//
// What it is, is a statement about work: prose-shaped content that happens to
// be structured. So it takes the document's serif one step down, on the quiet
// tint the document already gives quoted material, and keeps mono for exactly
// the things that are literals — the key and the commands.
//
// **Read-only, and that is the design (§8.3), not a missing slice.** The
// legacy card carried Release / Delete / Restore buttons; those are writes to
// the wave's task graph, and a report is an account of what happened, not the
// console you drive it from. What this renders is exactly what the block
// declares: who asked for it, whether it is ready to queue, what it is for,
// and what would count as done.
//
// A withdrawn task keeps its row. It renders `Withdrawn` with both
// attributions intact — the task existed, other reports may cite its block id,
// and a document that silently dropped it would be lying about its own past.

import type { TaskBlockPayload } from '../../../../../core/domain/report.ts';
import { Icon } from '../../../ui/icon/public.tsx';
import styles from './task.module.css';

/*
 * ── The block folds ───────────────────────────────────────────────────────
 *
 * A report that declares a dozen tasks spends most of its length on them, and
 * a reader scanning for the prose between them has to scroll past every
 * `Done when` and every gate command to find it. So the block is a disclosure:
 * the head — `Task`, the key, the kind, and whether it is ready — is the part
 * that answers "which task is this and does it need me", and it is the part
 * that stays.
 *
 * `<details>`/`<summary>`, not a `useState` and a button. The element already
 * is this control: it opens on Enter *and* Space, exposes `aria-expanded` off
 * its own open state with no attribute to keep in sync, is findable by the
 * browser's own in-page search (Chromium expands a closed `<details>` to reveal
 * a match, which a `hidden` div would not), and prints expanded. Every one of
 * those is a thing a hand-rolled version has to remember and this one cannot
 * forget.
 *
 * **Closed by default — and this reversed once, deliberately.** The first
 * round of this shipped `open`, on the argument that a report is an account and
 * that defaulting to folded would hide, from a reader who never asked, content
 * the document chose to state. That argument was right for the shape it was
 * written against: each task was a fold sitting *in the middle of the prose*,
 * so a closed one was a hole in the document's own argument.
 *
 * The shape changed. `features/report/document` now lifts every task out of the
 * flow into one collapsed `Reference` section at the end, so what a closed row
 * hides is no longer part of the argument — it is the machinery behind it, and
 * the reader who opened the appendix asked for a list, not for eight worker
 * prompts. Left open, opening `Reference` would have dumped the ~7400
 * characters this whole change exists to get out of the reading column.
 *
 * So the row is the one-line reference and the block is what it points at.
 * State is per-block and lives in the DOM — nothing is persisted, so a reload
 * is a fresh read of the document rather than a replay of how somebody last
 * folded it.
 */

const DECLARED_BY: Readonly<Record<'spec' | 'user', string>> =
  Object.freeze({ spec: 'Spec agent', user: 'You' });

type WithdrawnTask = Extract<TaskBlockPayload, { tombstoned_by: unknown }>;
type LiveTask = Exclude<TaskBlockPayload, WithdrawnTask>;

/** `tombstoned_by` is the discriminant, not `tombstone`: a live task may carry
 *  an explicit `tombstone: null`, so presence of that key proves nothing. */
function isWithdrawn(payload: TaskBlockPayload): payload is WithdrawnTask {
  return 'tombstoned_by' in payload;
}

export function ReportTaskBlock({ payload }: { payload: TaskBlockPayload }) {
  if (isWithdrawn(payload)) {
    const reason = payload.tombstone.reason;
    return (
      <details className={styles.task} data-nc-task-state="withdrawn">
        <summary className={styles.head}>
          <span className={styles.marker}><Icon name="chevron-right" size="sm" /></span>
          <span className={styles.kindLabel}>Task</span>
          <span className={styles.key}>{payload.key}</span>
          <span className={styles.spacer} />
          <span className={styles.withdrawn}>Withdrawn</span>
        </summary>
        {reason !== null && reason !== undefined && reason !== '' && (
          <p className={styles.goal}>{reason}</p>
        )}
        <dl className={styles.fields}>
          <dt className={styles.label}>Declared by</dt>
          <dd className={styles.value}>{DECLARED_BY[payload.declared_by]}</dd>
          <dt className={styles.label}>Withdrawn by</dt>
          <dd className={styles.value}>{DECLARED_BY[payload.tombstoned_by]}</dd>
        </dl>
      </details>
    );
  }

  const live: LiveTask = payload;
  return (
    <details className={styles.task} data-nc-task-state={live.ready ? 'ready' : 'not-ready'}>
      <summary className={styles.head}>
        {/* The disclosure's own glyph. `<summary>` draws a marker only while it
            is `display: list-item`, and this row is a flex box, so the platform
            triangle is gone and this is what replaces it — the rail's chevron,
            rotated by the same rule, because a disclosure is a disclosure
            wherever it appears. */}
        <span className={styles.marker}><Icon name="chevron-right" size="sm" /></span>
        {/* The block says what it is. Every other kind announces itself by its
            own shape — a table is a table — but a task is a paragraph of
            structured text, and without the word it reads as prose that has
            gone strange. In sentence case: the uppercase version of this label
            was the chrome vocabulary leaking into the document. */}
        <span className={styles.kindLabel}>Task</span>
        <span className={styles.key}>{live.key}</span>
        <span className={styles.spacer} />
        <span className={styles.kind}>
          {live.kind}{live.spawn === 'sub-wave' ? ' · sub-wave' : ''}
        </span>
        {/* Ready is a fact about the task, carried in words. It is not a badge:
            §6.6 spends a pill on lifecycle, and a second pill on this page
            would make two different things look like one kind of thing. */}
        <span className={live.ready ? styles.ready : styles.notReady}>
          {live.ready ? 'Ready' : 'Not ready'}
        </span>
      </summary>

      <p className={styles.goal}>{live.goal}</p>

      {/*
        Label / value, one grid, one label column — the same shape the rest of
        the app gives a set of facts about one object. It replaces three
        different inline layouts (a run-in label, a two-column list, a bare
        line), which is what made this block look like it had been assembled
        out of whatever was to hand.
      */}
      <dl className={styles.fields}>
        {live.acceptance != null && live.acceptance !== '' && (
          <>
            <dt className={styles.label}>Done when</dt>
            <dd className={styles.value}>{live.acceptance}</dd>
          </>
        )}
        {live.gate != null && live.gate.steps.length > 0 && (
          <>
            <dt className={styles.label}>Checks</dt>
            <dd className={styles.value}>
              <ul>
                {live.gate.steps.map((step, index) => (
                  <li key={`${step.name}-${index}`} className={styles.step}>
                    <span className={styles.stepCmd}>{step.cmd}</span>
                  </li>
                ))}
              </ul>
            </dd>
          </>
        )}
        <dt className={styles.label}>Declared by</dt>
        <dd className={styles.value}>{DECLARED_BY[live.declared_by]}</dd>
      </dl>
    </details>
  );
}
