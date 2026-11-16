import { MarkdownBlocks, markdownInlineText } from '../../../ui/markdown/public.tsx';
import { parse, sanitizeAstPolicy } from '../../../../../core/markdown/public.ts';
import { createElement, memo, useMemo, type ReactNode } from 'react';
import { Code } from '@astryxdesign/core/Code';
import { OutputImage, createOutputMediaOwner, type OutputMediaOwner } from './output-media.tsx';
import type { OutputProjection } from '../../../../../core/domain/conversation-output.ts';

import type { WorkspaceFilePort } from '../../../../../core/domain/fs.ts';
import { useState } from '../../../ui/state/public.ts';
import styles from './reply.module.css';

/** Bound by the app to the conversation's own Track. The server owns file containment. */
export type ReplyLinkRenderer = (props: Readonly<{ href: string; children: ReactNode }>) => ReactNode;

export type ReplyImageFiles = Readonly<{
  root: string;
  files: Pick<WorkspaceFilePort, 'rawUrl'>;
}>;

/** Stored and streamed replies share one renderer and Track-scoped capabilities. */
export const Reply = memo(function Reply({ text, output, mediaOwner }: { text: string; imageFiles: ReplyImageFiles | null; renderLink?: ReplyLinkRenderer; output?:OutputProjection; mediaOwner?:OutputMediaOwner }) {
  const [localOwner]=useState(createOutputMediaOwner);
  const owner=mediaOwner ?? localOwner;
  if(output !== undefined)return <OutputBody output={output} owner={owner} />;
  return <ReplyMarkdown text={text} />;
});

function ReplyMarkdown({text}:{text:string}) {
  const result = useMemo(() => parse(text), [text]);
  if (result.status === 'failed') return <pre className={styles.raw}>{text}</pre>;
  const ast = sanitizeAstPolicy(result.value, { rawHtml: 'literal' });
  const headingIds = new Map<number, string>();
  const counts = new Map<string, number>();
  for (const block of ast.children) {
    if (block.type !== 'heading') continue;
    const slug = markdownInlineText(block.children, '').trim().toLowerCase().replace(/['"]/g, '')
      .replace(/[^a-z0-9]+/g, '-').replace(/^-+|-+$/g, '') || 'section';
    const count = counts.get(slug) ?? 0;
    counts.set(slug, count + 1);
    headingIds.set(block.position.start.offset, count === 0 ? slug : `${slug}-${count}`);
  }
  return <MarkdownBlocks blocks={ast.children} presentation={{
    classes: { paragraph: styles.paragraph, list: styles.list, item: styles.item, check: styles.check,
      quote: styles.quote, inlineCode: styles.inlineCode, tableWrap: styles.tableWrap, table: styles.table,
      cell: styles.cell, rule: styles.rule },
    heading: (depth, offset, children) => createElement(`h${Math.min(depth + 2, 6)}`, { id: headingIds.get(offset), className: `${styles.heading} ${depth <= 3 ? styles.majorHeading : styles.minorHeading}` }, children),
    link: (href, _label, children) => {
      // Browsers ignore control characters within a scheme. Never turn unsafe destinations into links.
      const normalized = Array.from(href).filter(char => char.charCodeAt(0) > 31 && char.charCodeAt(0) !== 127).join('').trim();
      if (normalized === '' || /^(?:javascript|vbscript|data):/i.test(normalized)) return children;
      return outputLink(normalized,children);
    },
    image: (_src, alt) => <span className={styles.unavailable}>Image unavailable: {alt || 'Markdown image'}</span>,
  }} />;
}


function outputLink(href:string,children:ReactNode):ReactNode {
  if(!/^https?:\/\//i.test(href) || Array.from(href).some(char=>char.charCodeAt(0)<=32 || char.charCodeAt(0)===127))return <span>{children}</span>;
  return <a className={styles.link} href={href} target="_blank" rel="noopener noreferrer">{children}</a>;
}
function OutputBody({output,owner}:{output:OutputProjection;owner:OutputMediaOwner}) {
  if(output.kind === 'unavailable')return <span className={styles.unavailable}>Output unavailable</span>;
  return output.content.blocks.map((block,index)=>{
    const key=`${output.identity}/blocks/${index}`;
    switch(block.type){
      case 'text': return <ReplyMarkdown key={key} text={block.text} />;
      case 'resource_link': return <p key={key}>{outputLink(block.uri,block.name)}</p>;
      case 'resource': return 'text' in block.resource ? <Code key={key}>{block.resource.text}</Code>
        : <span key={key} className={styles.unavailable}>Unsupported binary resource · {block.resource.uri}</span>;
      case 'image': return <OutputImage key={key} identity={key} data={block.data} mime={block.mimeType}
        alt={typeof block.fields.name === 'string' ? block.fields.name : 'Output image'} owner={owner} />;
      case 'tool_diff': return <Code key={key}>{block.newText}</Code>;
      case 'audio': return <span key={key} className={styles.unavailable}>Unsupported audio · {block.mimeType}</span>;
      case 'terminal_reference': return <span key={key} className={styles.unavailable}>Unsupported terminal reference · {block.terminalId}</span>;
      case 'unknown_extension': return <span key={key} className={styles.unavailable}>Unsupported output extension</span>;
    }
  });
}
