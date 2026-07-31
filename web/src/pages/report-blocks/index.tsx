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
import ReactMarkdown, {
  defaultUrlTransform,
  type Components,
} from 'react-markdown';
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

const BLOCK_ID_PATTERN = /^b_[0-9a-f]{4}$/;

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

// Proposal-safe prose policy (issue #955 §5.4/§5.6) — PREVIEW ONLY.
//
// In the accepted report, markdown images and links are ordinary content:
// a human put them there. In a PENDING proposal they are not — the
// markdown is unadjudicated plugin text, and `![](https://x/px.png?who=…)`
// makes the browser perform a plugin-chosen request the instant the
// adjudicator merely LOOKS at the proposal (a zero-click view beacon,
// carrying IP / UA / Referer), before any accept. Links are live
// navigation into a plugin-chosen destination.
//
// So in the preview both degrade to inert descriptors that show the URL
// as TEXT: nothing loads, nothing navigates, and the adjudicator can
// actually see where the content points before deciding. This policy is
// scoped to the `preview` flag on purpose — the accepted report's
// rendering is untouched.
const previewProseComponents: Components = {
  img({ src, alt }) {
    const url = typeof src === 'string' ? src : '';
    return (
      <span className="rb-inert-media">
        image not loaded in this preview
        {alt ? ` — ${alt}` : ''} <code>{url}</code>
      </span>
    );
  },
  a({ href, children }) {
    const url = typeof href === 'string' ? href : '';
    return (
      <span className="rb-inert-link">
        {children} <code>{url}</code>
      </span>
    );
  },
};

export function ReportBlockView({
  block,
  preview = false,
}: {
  block: ReportBlock;
  /** Render for a PENDING proposal pane: media and links are inert. */
  preview?: boolean;
}) {
  switch (block.kind) {
    case 'prose': {
      const parsed = proseBlockPayloadSchema.safeParse(block.payload);
      if (!parsed.success) return <UnsupportedBlock block={block} />;
      return (
        <div id={block.id} className="report-block report-prose calm-prose">
          <ReactMarkdown
            remarkPlugins={[remarkGfm]}
            urlTransform={reportUrlTransform}
            components={preview ? previewProseComponents : { a: ReportLink }}
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
