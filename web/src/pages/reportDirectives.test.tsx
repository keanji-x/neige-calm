import { render, screen } from '@testing-library/react';
import ReactMarkdown from 'react-markdown';
import remarkDirective from 'remark-directive';
import remarkGfm from 'remark-gfm';
import { describe, expect, it } from 'vitest';
import {
  reactMarkdownComponents,
  remarkReportDirectives,
} from './reportDirectives';

function renderMarkdown(body: string) {
  return render(
    <div className="report-prose">
      <ReactMarkdown
        components={reactMarkdownComponents}
        remarkPlugins={[remarkGfm, remarkDirective, remarkReportDirectives]}
        remarkRehypeOptions={{ clobberPrefix: '' }}
      >
        {body}
      </ReactMarkdown>
    </div>,
  );
}

describe('reportDirectives', () => {
  it('renders :::findings as a findings card with a header', () => {
    const { container } = renderMarkdown(':::findings\nLead.\n:::\n');

    const findings = container.querySelector('.findings');
    expect(findings).toBeInTheDocument();
    expect(findings?.querySelector('.fh')).toHaveTextContent('Key findings');
  });

  it('renders row directives inside findings with stat, unit, and prose', () => {
    const { container } = renderMarkdown(
      ':::findings\n::row[First **increase**.]{stat="+6.2%" unit="QoQ"}\n:::\n',
    );

    const row = container.querySelector('.find-row');
    expect(row).toBeInTheDocument();
    expect(row?.querySelector('.find-stat')).toHaveTextContent('+6.2%');
    expect(row?.querySelector('.find-stat .u')).toHaveTextContent('QoQ');

    const text = row?.querySelector('.find-txt');
    expect(text).toHaveTextContent('First increase.');
    expect(text?.querySelector('strong')).toHaveTextContent('increase');
  });

  it('renders chart directives with label, caption, and figure caption', () => {
    const { container } = renderMarkdown(
      ':::chart{label="L" caption="C"}\nFig 1. Placeholder.\n:::\n',
    );

    const chart = container.querySelector('.chart');
    expect(chart).toBeInTheDocument();
    expect(chart?.querySelector('.ph-tag')).toHaveTextContent('L');
    expect(chart?.querySelector('.cap')).toHaveTextContent('C');
    expect(container.querySelector('.figcap')).toHaveTextContent(
      'Fig 1. Placeholder.',
    );
  });

  it('renders unknown directives as plain text without element or attribute leakage', () => {
    const source = ':::evil{onclick="alert(1)" style="x"}\nbad\n:::\n';
    const { container } = renderMarkdown(source);

    expect(container.querySelector('.evil')).not.toBeInTheDocument();
    expect(container.querySelector('[onclick]')).not.toBeInTheDocument();
    expect(container.querySelector('[style]')).not.toBeInTheDocument();
    expect(container).toHaveTextContent(':::evil{onclick="alert(1)" style="x"}');
    expect(container).toHaveTextContent('bad');
  });

  it('drops dangerous attributes on whitelisted directives', () => {
    const { container } = renderMarkdown(
      ':::chart{label="OK" caption="C" onclick="alert(1)" style="x" srcdoc="y"}\nFig.\n:::\n',
    );

    const chart = container.querySelector('.chart');
    expect(chart).toBeInTheDocument();
    expect(screen.getByText('OK')).toHaveClass('ph-tag');
    expect(screen.getByText('C')).toHaveClass('cap');
    expect(container.querySelector('[onclick]')).not.toBeInTheDocument();
    expect(container.querySelector('[style]')).not.toBeInTheDocument();
    expect(container.querySelector('[srcdoc]')).not.toBeInTheDocument();
  });
});
