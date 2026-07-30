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

// lightweight-charts (~45KB gz) only loads when a report actually carries a
// candle chart — same pattern as the lazily loaded CodeMirror pane.
const LazyCandlesBlock = lazy(() =>
  import('./candles').then((m) => ({ default: m.ReportCandlesBlock })),
);

function UnsupportedBlock({ block }: { block: ReportBlock }) {
  return (
    <div id={block.id} className="report-block rb-unsupported" role="note">
      unsupported block kind {block.kind}
    </div>
  );
}

export function reportUrlTransform(url: string): string {
  return url.startsWith('neige:') ? url : defaultUrlTransform(url);
}

function ReportLink({
  href,
  children,
}: React.ComponentPropsWithoutRef<'a'>) {
  const match = href?.match(/^neige:\/\/wave\/([^/?#]+)(?:#([^#]+))?$/);
  if (!match) return <a href={href}>{children}</a>;
  const [, waveId, blockId] = match;
  let hash: string | undefined = blockId;
  try {
    hash = blockId ? decodeURIComponent(blockId) : undefined;
  } catch {
    // Preserve a malformed-but-harmless anchor verbatim.
  }
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
      return (
        <div id={block.id} className="report-block report-prose calm-prose">
          <ReactMarkdown
            remarkPlugins={[remarkGfm]}
            urlTransform={reportUrlTransform}
            components={{ a: ReportLink }}
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
