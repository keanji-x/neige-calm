// The report document — the main column on a wave and a cove.
//
// **A report is a sequence of typed blocks, not one Markdown string** (§8.3).
// `prose` blocks render through `core/markdown`'s sanitized AST; `table`,
// `chart.candles`, `task` and `app` render themselves.
//
// **Every block is the same width.** The blocks used to break out from the
// prose measure to the full document width, on the theory that a table wants
// room; what that actually produced was a document with two left-to-right
// rhythms, where the eye had to re-find the column at every figure. One
// measure for every kind is the simpler promise, and the one a reader can see.
//
// A v1 report has no blocks, only the flat `body` projection. That renders as
// a single prose block, which is exactly what it is; it simply has no block
// ids, so it has no outline and no deep links.
//
// Raw HTML is dropped by `sanitizeAstPolicy`, so nothing reaches the DOM that
// the parser did not classify. There is no `dangerouslySetInnerHTML` anywhere
// on this path — every node below is a React element built from typed fields.
//
// It lives in `features/` and not `ui/`: it reads `core/domain` and
// `core/markdown`, and `ui/**` may import `core` *types* from three files only.
// The cove and wave pages therefore receive it by injection from `app/router`,
// the same way they receive the wave list.

import { useEffect, type ReactNode } from 'react';

import {
  extractOutline, parse, REPORT_MAX_DEPTH, reportHeadingIdPolicy, sanitizeAstPolicy,
  type SafeBlock, type SafeInline,
} from '../../../../../core/markdown/public.ts';
import {
  parseReportLink, type ReportBlock, type ReportLinkTarget, type WaveReport,
} from '../../../../../core/domain/report.ts';
import { Icon } from '../../../ui/icon/public.tsx';
import { revealReportAnchor } from '../anchor/public.ts';
import { ReportAppBlock } from '../app/public.tsx';
import { ReportCandlesBlock } from '../candles/public.tsx';
import { ReportTableBlock } from '../table/public.tsx';
import { ReportTaskBlock } from '../task/public.tsx';
import styles from './document.module.css';

export type ReportDocumentProps = Readonly<{
  /** The wave's report. `null` when it has none. */
  report: WaveReport | null;
  /** What the empty state should offer, which differs per route. */
  empty: ReactNode;
  /** The outline rail (§6.16), which hangs in the document's leading gutter. */
  rail?: ReactNode;
  /** The byline row (`Spec Agent · 2h`), composed by `app/router`. */
  byline?: ReactNode;
  /** How many backlinks land on each block, for the sidenote markers. */
  backlinkCounts?: ReadonlyMap<string, number>;
  /** A `neige://wave/…` link was activated. Absent ⇒ links render as plain text. */
  onOpenLink?: (target: ReportLinkTarget) => void;
  /** The anchor the reader arrived at, from a deep link or a backlink. */
  arrivalAnchorId?: string | null;
}>;

/**
 * INV-A11Y-061 — a report is prose, not navigation. It emits no `<a href>`:
 * a `neige://` link becomes a `<button>` plus a callback, and every other link
 * keeps its label and drops its destination. That keeps this surface
 * consistent with every other one in the app rather than making the document
 * the single place a native link exists.
 */
export function ReportDocument({
  report, empty, rail, byline, backlinkCounts, onOpenLink, arrivalAnchorId,
}: ReportDocumentProps) {
  useEffect(() => {
    if (arrivalAnchorId === null || arrivalAnchorId === undefined) return;
    revealReportAnchor(arrivalAnchorId);
  }, [arrivalAnchorId]);

  if (report === null) return <>{empty}</>;

  return (
    /* `calm-prose` is the app's one prose recipe (base.css) — measure, serif,
       size, leading, and the block rhythm. The module class beside it adds only
       what is specific to a *report*: the numbered sections, the outline
       gutter and the sidenote. Two prose definitions is how the question
       "what does prose look like" stops having one answer. */
    <article className={`calm-prose ${styles.doc}`} data-nc-report="">
      {rail}
      {byline !== undefined && <div className={styles.byline}>{byline}</div>}
      {report.blocks === null
        ? (
          // A v1 report is one prose block that happens to have no id: same
          // column, same measure, just nothing to anchor to.
          <div className={styles.row}>
            <div className={styles.block}>
              <ProseBlock markdown={report.body || report.summary} blockId={null} onOpenLink={onOpenLink} />
            </div>
          </div>
        )
        : (() => {
          /*
           * ── The document, then the reference section ──────────────────────
           *
           * The report is meant to be a deliverable, and it was not reading as
           * one. Measured on a real wave: 8141 characters of body, 11 blocks —
           * four prose and seven `task`. The prose a reader is meant to take
           * away was about 700 characters; the rest was worker prompts,
           * acceptance criteria and gate shell commands, set inline at content
           * weight, between the paragraphs that were the actual conclusions.
           *
           * Those are not the report's argument, they are the machinery that
           * produced it. So they come out of the flow and go to the end, inside
           * one collapsed section. The reading column is then what the wave
           * *concluded*; the appendix is how it got there, one line each, for
           * the reader who wants to check.
           *
           * **`processBlocks` is a predicate, not a list of tasks.** The
           * section is named `Reference` rather than `Tasks` because the split
           * it draws is "argument / machinery", and everything procedural the
           * report grows later belongs on the same side of it. Adding a kind
           * here is the whole change.
           */
          const documentBlocks = report.blocks.filter((block) => !isProcessBlock(block));
          const processBlocks = report.blocks.filter(isProcessBlock);
          return (
            <>
              {documentBlocks.map((block) => (
                <BlockSlot
                  key={block.id}
                  block={block}
                  backlinks={backlinkCounts?.get(block.id) ?? 0}
                  onOpenLink={onOpenLink}
                />
              ))}
              {/* §6.1 — a section with zero rows is not rendered. A report that
                  declared no tasks has no machinery to account for, and a
                  permanent empty appendix would make that look like a gap. */}
              {processBlocks.length > 0 && (
                <ReportReference blocks={processBlocks} backlinkCounts={backlinkCounts} />
              )}
            </>
          );
        })()}
    </article>
  );
}

/** Blocks that record how the work was driven rather than what it concluded.
 *  Today that is `task`; the predicate exists so the next one is a one-line
 *  change rather than a second mechanism. */
function isProcessBlock(block: ReportBlock): boolean {
  return block.kind === 'task';
}

/**
 * The appendix, closed.
 *
 * **Closed by default, and the earlier argument against that does not carry
 * over.** When each task was its own fold in the middle of the prose, opening
 * closed would have been the app deciding, for a reader who never asked, that
 * part of the document's own argument was not worth showing. This is the
 * opposite shape: one labelled section, at the end, holding only the things
 * that are *not* the argument. Nothing the report concluded is behind it.
 *
 * One fold, not N. Eight collapsed task rows scattered through the prose still
 * cost eight interruptions and eight decisions; this costs one, and the reader
 * who never opens it reads a clean document.
 *
 * The rows inside keep their block ids, which is what makes this safe: a
 * `neige://wave/x#b_task` link from another report, and the panel's task
 * inventory, both still land — `revealReportAnchor` opens every `<details>` it
 * lands inside before it measures where to scroll, so arriving here unfolds
 * the section and the row together.
 */
function ReportReference({ blocks, backlinkCounts }: {
  blocks: readonly ReportBlock[];
  backlinkCounts?: ReadonlyMap<string, number>;
}) {
  return (
    <div className={styles.row}>
      <details className={styles.reference} data-nc-report-reference="">
        <summary className={styles.referenceHead}>
          <span className={styles.referenceMarker}><Icon name="chevron-right" size="sm" /></span>
          <span className={styles.referenceTitle}>Reference</span>
          {/* The count is the reason to open it, and the only thing the closed
              row can say about what is inside. Tabular figures so a document
              with two of these does not jitter. */}
          <span className={styles.referenceCount}>
            {blocks.length} {blocks.length === 1 ? 'task' : 'tasks'}
          </span>
        </summary>
        {/*
          * Not `BlockSlot`, and that is a layout fact rather than a preference.
          * A slot is `display: contents` over the *article's* three-column grid
          * — leading gutter, measure, sidenote gutter — and this `<details>` is
          * not that grid, so a slot nested here would drop its `grid-column: 2`
          * and the backlink marker would have no gutter to sit in. The entries
          * therefore carry the two things a slot exists for, directly: the block
          * id (which is the anchor every deep link and the TASKS panel address)
          * and the backlink count, which moves inline because there is no
          * trailing gutter inside an appendix.
          */}
        {blocks.map((block) => {
          const backlinks = backlinkCounts?.get(block.id) ?? 0;
          return (
            <div key={block.id} className={styles.referenceItem} id={block.id}>
              <BlockBody block={block} />
              {backlinks > 0 && (
                <span
                  className={styles.referenceSidenote}
                  title={`${backlinks} report${backlinks === 1 ? '' : 's'} cite this block`}
                >
                  ◂ {backlinks}
                </span>
              )}
            </div>
          );
        })}
      </details>
    </div>
  );
}

/**
 * One block, plus the sidenote that belongs to it.
 *
 * The slot — not the block renderer — owns the id and the column, so every
 * kind lands on the same left edge and every kind can be cited by id without
 * each renderer having to remember to carry one.
 */
function BlockSlot({ block, backlinks, onOpenLink }: {
  block: ReportBlock;
  backlinks: number;
  onOpenLink?: (target: ReportLinkTarget) => void;
}) {
  return (
    <div className={styles.row}>
      <div className={styles.block} id={block.id}>
        {block.kind === 'prose'
          ? <ProseBlock markdown={block.payload.markdown} blockId={block.id} onOpenLink={onOpenLink} />
          : <BlockBody block={block} />}
      </div>
      {backlinks > 0 && (
        // In the trailing gutter, aligned to the block's first line. Inside the
        // prose it would interrupt the reading; out here peripheral vision
        // finds it and nothing is in the way if you ignore it.
        <span className={styles.sidenote} title={`${backlinks} report${backlinks === 1 ? '' : 's'} cite this block`}>
          ◂ {backlinks}
        </span>
      )}
    </div>
  );
}

/** One bad block may not cost the page: an unknown kind, or a known kind whose
 *  payload did not parse, degrades to one line and the document goes on. */
function BlockBody({ block }: { block: ReportBlock }): ReactNode {
  switch (block.kind) {
    case 'table': return <ReportTableBlock payload={block.payload} />;
    case 'chart.candles': return <ReportCandlesBlock payload={block.payload} />;
    case 'task': return <ReportTaskBlock payload={block.payload} />;
    case 'app': return <ReportAppBlock payload={block.payload} />;
    case 'unsupported':
      return (
        <div className={styles.unsupported} role="note">
          unsupported block kind {block.declaredKind}
        </div>
      );
    // `prose` is handled by the slot, which owns the markdown pipeline.
    case 'prose': return null;
  }
}

function ProseBlock({ markdown, blockId, onOpenLink }: {
  markdown: string;
  blockId: string | null;
  onOpenLink?: (target: ReportLinkTarget) => void;
}) {
  const parsed = parse(markdown);
  // A report that will not parse is still a report; showing the source beats
  // showing an error, because the source is what the agent actually wrote.
  if (parsed.status === 'failed') {
    return <pre className={styles.raw}>{markdown}</pre>;
  }

  /*
   * Heading ids come from the same `extractOutline` call the outline itself
   * uses, keyed by source offset. Counting headings a second time here would
   * be a second implementation of "which headings are sections", and the two
   * would drift the first time the policy changed — leaving an outline whose
   * entries scroll to nothing.
   */
  const headingIds = blockId === null
    ? new Map<number, string>()
    : new Map(extractOutline([{ context: { blockId }, ast: parsed.value }], {
      maxDepth: REPORT_MAX_DEPTH,
      headingId: reportHeadingIdPolicy,
      textPolicy: 'non-empty-heading-label',
      referenceText: 'visible',
      traversal: 'recursive',
    }).map((heading) => [heading.position.start.offset ?? -1, heading.id]));

  const ast = sanitizeAstPolicy(parsed.value, { rawHtml: 'drop' });
  return <>{ast.children.map((block, index) => (
    <Block key={index} block={block} headingIds={headingIds} onOpenLink={onOpenLink} />
  ))}</>;
}

type BlockContext = Readonly<{
  headingIds: ReadonlyMap<number, string>;
  onOpenLink?: (target: ReportLinkTarget) => void;
}>;

function Block({ block, headingIds, onOpenLink }: { block: SafeBlock } & BlockContext): ReactNode {
  const context: BlockContext = { headingIds, onOpenLink };
  switch (block.type) {
    case 'heading': {
      // The kernel derives sections by splitting at H1, so H1 is a *section*
      // rule here, not a page title — the page title is the wave's name in the
      // header, and a document may not carry a second one. H1 and H2 are all
      // `REPORT_MAX_DEPTH` admits; anything deeper renders as H2 rather than
      // vanishing, because dropping a heading loses the text under it.
      const id = headingIds.get(block.position.start.offset ?? -1);
      if (block.depth !== 1) {
        return (
          <h3 className={styles.h2} id={id}>
            <Inlines nodes={block.children} {...context} />
          </h3>
        );
      }
      return (
        // The section's text is wrapped so the trailing rule can be a flex
        // sibling that takes the space the words leave. The rule used to be a
        // 100%-wide pseudo-element pulled back by a negative margin, which
        // worked only under a clip — and the clip would have taken the margin's
        // section number with it once that moved out of the heading.
        <h2 className={styles.h1} id={id}>
          <span className={styles.headingText}><Inlines nodes={block.children} {...context} /></span>
        </h2>
      );
    }
    case 'paragraph':
      return <p className={styles.p}><Inlines nodes={block.children} {...context} /></p>;
    case 'code':
      return <pre className={styles.code}><code>{block.value}</code></pre>;
    case 'blockquote':
      return (
        <blockquote className={styles.quote}>
          {block.children.map((child, index) => <Block key={index} block={child} {...context} />)}
        </blockquote>
      );
    case 'list': {
      const Tag = block.ordered ? 'ol' : 'ul';
      return (
        <Tag className={styles.list} start={block.ordered && block.start !== null ? block.start : undefined}>
          {block.children.map((item, index) => (
            <li key={index} className={styles.item}>
              {/* A task list keeps its box, disabled: the report states what was
                  done, and a checkbox you can tick would be claiming this
                  surface writes back. It does not. */}
              {item.checked !== null && (
                <input type="checkbox" className={styles.check} checked={item.checked} disabled readOnly />
              )}
              {item.children.map((child, childIndex) => (
                /*
                 * A *tight* list item renders its paragraph's inlines directly,
                 * with no `<p>` around them. That is Markdown's own rule, not a
                 * style choice: `spread` is exactly the parser telling us the
                 * author wrote the items on consecutive lines. Wrapping them in
                 * a block instead put every bullet's text on the line below its
                 * marker, and a task item's label below its checkbox.
                 */
                !block.spread && child.type === 'paragraph'
                  ? <Inlines key={childIndex} nodes={child.children} {...context} />
                  : <Block key={childIndex} block={child} {...context} />
              ))}
            </li>
          ))}
        </Tag>
      );
    }
    case 'table':
      return (
        // Its own scroll container: a wide table may not make the page scroll
        // sideways (§3.2).
        <div className={styles.tableWrap}>
          <table className={styles.table}>
            <tbody>
              {block.children.map((row, rowIndex) => (
                <tr key={rowIndex}>
                  {row.children.map((cell, cellIndex) => {
                    const Cell = rowIndex === 0 ? 'th' : 'td';
                    return (
                      <Cell
                        key={cellIndex}
                        className={styles.cell}
                        style={{ textAlign: block.align[cellIndex] ?? undefined }}
                      >
                        <Inlines nodes={cell.children} {...context} />
                      </Cell>
                    );
                  })}
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      );
    case 'thematicBreak':
      return <hr className={styles.rule} />;
  }
}

function Inlines({ nodes, ...context }: { nodes: readonly SafeInline[] } & BlockContext): ReactNode {
  return <>{nodes.map((node, index) => <Inline key={index} node={node} {...context} />)}</>;
}

function Inline({ node, headingIds, onOpenLink }: { node: SafeInline } & BlockContext): ReactNode {
  const context: BlockContext = { headingIds, onOpenLink };
  switch (node.type) {
    case 'text':
      return node.value;
    case 'inlineCode':
      return <code className={styles.inlineCode}>{node.value}</code>;
    case 'strong':
      return <strong className={styles.strong}><Inlines nodes={node.children} {...context} /></strong>;
    case 'emphasis':
      return <em><Inlines nodes={node.children} {...context} /></em>;
    case 'delete':
      return <del><Inlines nodes={node.children} {...context} /></del>;
    case 'break':
      return <br />;
    case 'link': {
      // A `neige://wave/…` link is the app's own citation and is the one link
      // that leads anywhere: it becomes a button, because INV-A11Y-061 puts
      // every navigation in the app through a button and a callback. Every
      // other destination is dropped and the label kept — a report is written
      // by an agent from sources this app cannot vouch for, and a bare
      // `<a href>` here would be the one place an untrusted URL reaches the
      // user's browser.
      const target = parseReportLink(node.destination);
      if (target === null || onOpenLink === undefined) {
        return <Inlines nodes={node.children} {...context} />;
      }
      return (
        <button type="button" className={styles.link} onClick={() => onOpenLink(target)}>
          <Inlines nodes={node.children} {...context} />
        </button>
      );
    }
    case 'image':
      // Same reason, and one more: an image loads its destination without a
      // click, so rendering one would fetch from wherever the report says.
      return <span className={styles.imageAlt}>{node.alt}</span>;
  }
}
