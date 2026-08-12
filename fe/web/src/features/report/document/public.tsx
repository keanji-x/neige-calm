// The report document — the main column on a wave and a cove.
//
// It renders `core/markdown`'s sanitized AST rather than a shape of its own.
// That module was written for exactly this (`REPORT_MAX_DEPTH`,
// `reportHeadingIdPolicy`) and had no consumer until now; a second Markdown
// vocabulary invented here would have been the third place in the repo that
// knows what a heading is.
//
// Raw HTML is dropped by `sanitizeAstPolicy`, so nothing reaches the DOM that
// the parser did not classify. There is no `dangerouslySetInnerHTML` anywhere
// on this path — every node below is a React element built from typed fields.
//
// It lives in `features/` and not `ui/`: it reads `core/domain` and
// `core/markdown`, and `ui/**` may import `core` *types* from three files only.
// The cove and wave pages therefore receive it by injection from `app/router`,
// the same way they receive the wave list.

import type { ReactNode } from 'react';

import {
  parse, sanitizeAstPolicy,
  type SafeBlock, type SafeInline,
} from '../../../../../core/markdown/public.ts';
import styles from './document.module.css';

export type ReportDocumentProps = Readonly<{
  /** Markdown source — `WaveReportPayload.body`. `null` when there is no report. */
  body: string | null;
  /** What the empty state should offer, which differs per route. */
  empty: ReactNode;
}>;

/**
 * INV-A11Y-061 — a report is prose, not navigation. It emits no `<a href>`:
 * links are dropped by the inline renderer below, which keeps this surface
 * consistent with every other one in the app rather than making the document
 * the single place a native link exists.
 */
export function ReportDocument({ body, empty }: ReportDocumentProps) {
  if (body === null) return <>{empty}</>;

  const parsed = parse(body);
  // A report that will not parse is still a report; showing the source beats
  // showing an error, because the source is what the agent actually wrote.
  if (parsed.status === 'failed') {
    return <pre className={styles.raw}>{body}</pre>;
  }

  const ast = sanitizeAstPolicy(parsed.value, { rawHtml: 'drop' });
  return (
    /* `calm-prose` is the app's one prose recipe (base.css) — measure, serif,
       size, leading, and the block rhythm. The module class beside it adds only
       what is specific to a *report*: the numbered sections. Two prose
       definitions is how the question "what does prose look like" stops having
       one answer. */
    <article className={`calm-prose ${styles.doc}`} data-nc-report="">
      {ast.children.map((block, index) => <Block key={index} block={block} />)}
    </article>
  );
}

function Block({ block }: { block: SafeBlock }): ReactNode {
  switch (block.type) {
    case 'heading': {
      // The kernel derives sections by splitting at H1, so H1 is a *section*
      // rule here, not a page title — the page title is the wave's name in the
      // header, and a document may not carry a second one. H1 and H2 are all
      // `REPORT_MAX_DEPTH` admits; anything deeper renders as H2 rather than
      // vanishing, because dropping a heading loses the text under it.
      const Tag = block.depth === 1 ? 'h2' : 'h3';
      const className = block.depth === 1 ? styles.h1 : styles.h2;
      return <Tag className={className}><Inlines nodes={block.children} /></Tag>;
    }
    case 'paragraph':
      return <p className={styles.p}><Inlines nodes={block.children} /></p>;
    case 'code':
      return <pre className={styles.code}><code>{block.value}</code></pre>;
    case 'blockquote':
      return (
        <blockquote className={styles.quote}>
          {block.children.map((child, index) => <Block key={index} block={child} />)}
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
                  ? <Inlines key={childIndex} nodes={child.children} />
                  : <Block key={childIndex} block={child} />
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
                        <Inlines nodes={cell.children} />
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

function Inlines({ nodes }: { nodes: readonly SafeInline[] }): ReactNode {
  return <>{nodes.map((node, index) => <Inline key={index} node={node} />)}</>;
}

function Inline({ node }: { node: SafeInline }): ReactNode {
  switch (node.type) {
    case 'text':
      return node.value;
    case 'inlineCode':
      return <code className={styles.inlineCode}>{node.value}</code>;
    case 'strong':
      return <strong className={styles.strong}><Inlines nodes={node.children} /></strong>;
    case 'emphasis':
      return <em><Inlines nodes={node.children} /></em>;
    case 'delete':
      return <del><Inlines nodes={node.children} /></del>;
    case 'break':
      return <br />;
    case 'link':
      // The destination is dropped, the label is kept. A report is written by
      // an agent from sources this app cannot vouch for, and INV-A11Y-061 puts
      // every navigation in the app through a button and a callback — a bare
      // `<a href>` here would be both the one exception and the one place an
      // untrusted URL reaches the user's browser.
      return <Inlines nodes={node.children} />;
    case 'image':
      // Same reason, and one more: an image loads its destination without a
      // click, so rendering one would fetch from wherever the report says.
      return <span className={styles.imageAlt}>{node.alt}</span>;
  }
}
