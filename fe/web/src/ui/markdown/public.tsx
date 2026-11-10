import { createElement, Fragment, type ReactNode } from 'react';
import katex from 'katex';
import type { MarkdownDepth, SafeBlock, SafeInline } from '../../../../core/markdown/public.ts';
import { ReadOnlyCode } from '../code/public.tsx';
import styles from './markdown.module.css';
import { flushTableEdgesClassName } from './table-edges.ts';

type Classes = Readonly<Partial<Record<'paragraph' | 'list' | 'item' | 'check' | 'quote' | 'inlineCode' | 'strong' | 'tableWrap' | 'table' | 'cell' | 'rule', string>>>;
export type MarkdownPresentation = Readonly<{
  classes?: Classes;
  heading?: (depth: MarkdownDepth, offset: number, children: ReactNode) => ReactNode;
  text?: (value: string, inLink: boolean) => ReactNode;
  inlineCode?: (value: string, inLink: boolean, body: ReactNode) => ReactNode;
  link?: (destination: string, label: string, children: ReactNode) => ReactNode;
  image?: (destination: string, alt: string, inLink: boolean) => ReactNode;
}>;

function Formula({ value, display }: Readonly<{ value: string; display: boolean; children?: string }>) {
  let html: string;
  try {
    html = katex.renderToString(value, { displayMode: display, throwOnError: true, trust: false, maxExpand: 1000, maxSize: 50 });
  } catch {
    return display ? <pre className={styles.invalid}>{value}</pre> : <code>{value}</code>;
  }
  const content = <span dangerouslySetInnerHTML={{ __html: html }} />;
  return display ? <div className={styles.display} data-nc-math-display="">{content}</div> : content;
}

export function markdownInlineText(nodes: readonly SafeInline[], breakText = ' '): string {
  return nodes.map(node => {
    switch (node.type) {
      case 'text': case 'inlineCode': case 'inlineMath': return node.value;
      case 'image': return node.alt;
      case 'break': return breakText;
      default: return markdownInlineText(node.children, breakText);
    }
  }).join('');
}

function Inlines({ nodes, presentation, inLink = false }: { nodes: readonly SafeInline[]; presentation: MarkdownPresentation; inLink?: boolean }) {
  return <>{nodes.map((node, index) => {
    let child: ReactNode;
    switch (node.type) {
      case 'text': child = presentation.text?.(node.value, inLink) ?? node.value; break;
      case 'inlineCode': {
        const code = <code className={presentation.classes?.inlineCode}>{node.value}</code>;
        child = presentation.inlineCode?.(node.value, inLink, code) ?? code;
        break;
      }
      case 'inlineMath': child = <Formula value={node.value} display={false}>{node.value}</Formula>; break;
      case 'break': child = <br />; break;
      case 'strong': child = <strong className={presentation.classes?.strong}>{Inlines({ nodes: node.children, presentation, inLink })}</strong>; break;
      case 'emphasis': child = <em>{Inlines({ nodes: node.children, presentation, inLink })}</em>; break;
      case 'delete': child = <del>{Inlines({ nodes: node.children, presentation, inLink })}</del>; break;
      case 'image': child = presentation.image?.(node.destination, node.alt, inLink) ?? node.alt; break;
      case 'link': {
        const body = Inlines({ nodes: node.children, presentation, inLink: true });
        child = inLink ? body : presentation.link?.(node.destination, markdownInlineText(node.children), body) ?? body;
        break;
      }
    }
    return <Fragment key={index}>{child}</Fragment>;
  })}</>;
}

/** Keep each column readable using the longest visible cell, including nested inline content. */
function tableColumnMinWidths(table: Extract<SafeBlock, { type: 'table' }>): readonly number[] {
  return (table.children[0]?.children ?? []).map((_, column) => {
    const length = Math.max(...table.children.map(row => markdownInlineText(row.children[column]?.children ?? []).length));
    return length <= 6 ? 60 : length <= 15 ? 80 : 120;
  });
}

function Block({ block, presentation }: { block: SafeBlock; presentation: MarkdownPresentation }): ReactNode {
  const classes = presentation.classes;
  switch (block.type) {
    case 'heading': {
      const children = <Inlines nodes={block.children} presentation={presentation} />;
      return presentation.heading?.(block.depth, block.position.start.offset, children)
        ?? createElement(`h${block.depth}`, null, children);
    }
    case 'paragraph': return <p className={classes?.paragraph}><Inlines nodes={block.children} presentation={presentation} /></p>;
    case 'code': return <ReadOnlyCode text={block.value} source={{ kind: 'language', value: block.language ?? '' }} />;
    case 'math': return <Formula value={block.value} display />;
    case 'blockquote': return <blockquote className={classes?.quote}>{block.children.map((child, index) => <Block key={index} block={child} presentation={presentation} />)}</blockquote>;
    case 'list': {
      const Tag = block.ordered ? 'ol' : 'ul';
      return <Tag className={classes?.list} start={block.ordered ? block.start ?? undefined : undefined}>
        {block.children.map((item, index) => <li key={index} className={classes?.item}>
          {item.checked !== null && <input type="checkbox" className={classes?.check} checked={item.checked} disabled readOnly />}
          {item.children.map((child, childIndex) => !block.spread && child.type === 'paragraph'
            ? <Inlines key={childIndex} nodes={child.children} presentation={presentation} />
            : <Block key={childIndex} block={child} presentation={presentation} />)}
        </li>)}
      </Tag>;
    }
    case 'table': {
      const minWidths = tableColumnMinWidths(block);
      return <div className={classes?.tableWrap}><table className={`${styles.table} ${flushTableEdgesClassName} ${classes?.table ?? ''}`}><tbody>
        {block.children.map((row, rowIndex) => <tr key={rowIndex}>{row.children.map((cell, cellIndex) => {
          const Cell = rowIndex === 0 ? 'th' : 'td';
          return <Cell key={cellIndex} className={classes?.cell} style={{ textAlign: block.align[cellIndex] ?? undefined, minWidth: minWidths[cellIndex] }}>
            <Inlines nodes={cell.children} presentation={presentation} />
          </Cell>;
        })}</tr>)}
      </tbody></table></div>;
    }
    case 'thematicBreak': return <hr className={classes?.rule} />;
  }
}

/** Render sanitized syntax; navigation, images and heading identity remain caller-owned. */
export function MarkdownBlocks({ blocks, presentation }: { blocks: readonly SafeBlock[]; presentation: MarkdownPresentation }) {
  return <>{blocks.map((block, index) => <Block key={index} block={block} presentation={presentation} />)}</>;
}
