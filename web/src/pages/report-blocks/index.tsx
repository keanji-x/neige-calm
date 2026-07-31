// Report block renderers (issue #960 PR3 S2).
//
// A wave report is a sequence of typed blocks. `prose` renders through the
// existing calm-prose markdown pipeline; `chart.candles` / `table` / `app`
// break out of the 616px prose measure to the full 748px document width
// while sharing the same left edge (flush-left — never centered
// independently). Unknown kinds and malformed payloads degrade to a small
// mono placeholder so one bad block never takes down the page.

import { lazy, Suspense } from 'react';
import { Link } from '@tanstack/react-router';
import { fromMarkdown } from 'mdast-util-from-markdown';
import ReactMarkdown, { defaultUrlTransform } from 'react-markdown';
import remarkGfm from 'remark-gfm';
import {
  appBlockPayloadSchema,
  chartCandlesPayloadSchema,
  proseBlockPayloadSchema,
  tableBlockPayloadSchema,
  type ReportBlock,
} from '../../cards/builtins/wave-report';
import { ReportTableBlock } from './table';
import { ReportAppBlock } from './app';
import { BLOCK_ID_PATTERN } from '../report-link-ids';
import { reportH2Id } from '../report-outline';

// lightweight-charts (~45KB gz) only loads when a report actually carries a
// candle chart — same pattern as the lazily loaded CodeMirror pane.
const LazyCandlesBlock = lazy(() =>
  import('./candles').then((m) => ({ default: m.ReportCandlesBlock })),
);

type PositionedMarkdownNode = {
  type?: unknown;
  depth?: unknown;
  position?: { start?: { offset?: unknown } };
  children?: unknown;
};

function collectH2Offsets(node: unknown): number[] {
  if (typeof node !== 'object' || node === null) return [];
  const candidate = node as PositionedMarkdownNode;
  const offset = candidate.position?.start?.offset;
  const ownOffset =
    candidate.type === 'heading' &&
    candidate.depth === 2 &&
    typeof offset === 'number'
      ? [offset]
      : [];
  const childOffsets = Array.isArray(candidate.children)
    ? candidate.children.flatMap(collectH2Offsets)
    : [];
  return [...ownOffset, ...childOffsets];
}

function UnsupportedBlock({ block }: { block: ReportBlock }) {
  return (
    <div id={block.id} className="report-block rb-unsupported" role="note">
      unsupported block kind {block.kind}
    </div>
  );
}

export function reportUrlTransform(url: string): string {
  const match = url.match(/^neige:\/\/wave\/([^/?#]+)(?:#([^#]+))?$/);
  if (!match) return defaultUrlTransform(url);
  const [, waveId, blockId] = match;
  return blockId && !BLOCK_ID_PATTERN.test(blockId)
    ? `neige://wave/${waveId}`
    : url;
}

export function ReportLink({
  href,
  children,
}: React.ComponentPropsWithoutRef<'a'>) {
  const match = href?.match(/^neige:\/\/wave\/([^/?#]+)(?:#([^#]+))?$/);
  if (!match) return <a href={href}>{children}</a>;
  const [, waveId, blockId] = match;
  const hash = blockId && BLOCK_ID_PATTERN.test(blockId) ? blockId : undefined;
  return (
    <Link
      to="/wave/$waveId"
      params={{ waveId }}
      hash={hash}
    >
      {children}
    </Link>
  );
}

export function ReportBlockView({ block }: { block: ReportBlock }) {
  switch (block.kind) {
    case 'prose': {
      const parsed = proseBlockPayloadSchema.safeParse(block.payload);
      if (!parsed.success) return <UnsupportedBlock block={block} />;
      const h2Offsets = collectH2Offsets(fromMarkdown(parsed.data.markdown));
      return (
        <div id={block.id} className="report-block report-prose calm-prose">
          <ReactMarkdown
            remarkPlugins={[remarkGfm]}
            urlTransform={reportUrlTransform}
            components={{
              a: ReportLink,
              h2: ({ children, node }) => {
                const offset = node?.position?.start.offset;
                const h2Index =
                  typeof offset === 'number' ? h2Offsets.indexOf(offset) : -1;
                return (
                  <h2
                    id={
                      h2Index >= 0
                        ? reportH2Id(block.id, h2Index)
                        : undefined
                    }
                  >
                    {children}
                  </h2>
                );
              },
            }}
          >
            {parsed.data.markdown}
          </ReactMarkdown>
        </div>
      );
    }
    case 'chart.candles': {
      const parsed = chartCandlesPayloadSchema.safeParse(block.payload);
      if (!parsed.success) return <UnsupportedBlock block={block} />;
      return (
        <div id={block.id} className="report-block report-block--breakout">
          <Suspense
            fallback={
              <div className="rb-unsupported" role="status">
                Loading chart…
              </div>
            }
          >
            <LazyCandlesBlock payload={parsed.data} />
          </Suspense>
        </div>
      );
    }
    case 'table': {
      const parsed = tableBlockPayloadSchema.safeParse(block.payload);
      if (!parsed.success) return <UnsupportedBlock block={block} />;
      return (
        <div id={block.id} className="report-block report-block--breakout">
          <ReportTableBlock payload={parsed.data} />
        </div>
      );
    }
    case 'app': {
      const parsed = appBlockPayloadSchema.safeParse(block.payload);
      if (!parsed.success) return <UnsupportedBlock block={block} />;
      return (
        <div id={block.id} className="report-block report-block--breakout">
          <ReportAppBlock payload={parsed.data} />
        </div>
      );
    }
    default:
      return <UnsupportedBlock block={block} />;
  }
}
