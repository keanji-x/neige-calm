import { describe, it, expect, vi } from 'vitest';
import { render, fireEvent } from '@testing-library/react';
import {
  extractHeadings,
  MarkdownToc,
  tocHeadingId,
  type TocHeading,
} from './file-viewer-markdown-toc';

describe('extractHeadings', () => {
  it('returns [] for empty input', () => {
    expect(extractHeadings('')).toEqual([]);
  });

  it('captures h1–h4 with positional ids in document order', () => {
    const src = [
      '# One',
      '## Two',
      '### Three',
      '#### Four',
    ].join('\n');
    expect(extractHeadings(src)).toEqual([
      { level: 1, text: 'One', id: 'md-h-0' },
      { level: 2, text: 'Two', id: 'md-h-1' },
      { level: 3, text: 'Three', id: 'md-h-2' },
      { level: 4, text: 'Four', id: 'md-h-3' },
    ]);
  });

  it('ignores h5 and deeper', () => {
    const src = ['# ok', '##### skipme', '###### also skip'].join('\n');
    const out = extractHeadings(src);
    expect(out.map((h) => h.text)).toEqual(['ok']);
  });

  it('skips `#`-lines inside ``` fenced code blocks', () => {
    const src = [
      '# real',
      '```',
      '# not a heading',
      '## also not',
      '```',
      '## after',
    ].join('\n');
    const out = extractHeadings(src);
    expect(out.map((h) => h.text)).toEqual(['real', 'after']);
  });

  it('skips `#`-lines inside ~~~ fenced code blocks', () => {
    const src = [
      '# real',
      '~~~',
      '# not a heading',
      '~~~',
      '## after',
    ].join('\n');
    expect(extractHeadings(src).map((h) => h.text)).toEqual(['real', 'after']);
  });

  it('strips inline bold / italic / code / links from heading text', () => {
    const src = [
      '# **Bold** heading',
      '## `code` and _em_ mixed',
      '### [link text](https://example.com/path)',
    ].join('\n');
    expect(extractHeadings(src).map((h) => h.text)).toEqual([
      'Bold heading',
      'code and em mixed',
      'link text',
    ]);
  });

  it('trims trailing closing hashes (## Foo ##)', () => {
    expect(extractHeadings('## Foo ##').map((h) => h.text)).toEqual(['Foo']);
  });

  it('requires a space between hashes and text (no ATX-atomic mash)', () => {
    // `#foo` (no space) is not a heading in CommonMark; regex declines it.
    expect(extractHeadings('#foo').map((h) => h.text)).toEqual([]);
  });

  it('exposes tocHeadingId as the id scheme', () => {
    expect(tocHeadingId(0)).toBe('md-h-0');
    expect(tocHeadingId(7)).toBe('md-h-7');
  });

  it('captures setext h1 (===) and h2 (---) with the paragraph text above', () => {
    const src = ['Alpha', '=====', '', 'Beta', '-----'].join('\n');
    expect(extractHeadings(src)).toEqual([
      { level: 1, text: 'Alpha', id: 'md-h-0' },
      { level: 2, text: 'Beta', id: 'md-h-1' },
    ]);
  });

  it('interleaves setext and atx headings while keeping ids positional', () => {
    const src = [
      'Setext H1',
      '===',
      '',
      '# Atx H1',
      '',
      '## Atx H2',
    ].join('\n');
    expect(extractHeadings(src)).toEqual([
      { level: 1, text: 'Setext H1', id: 'md-h-0' },
      { level: 1, text: 'Atx H1', id: 'md-h-1' },
      { level: 2, text: 'Atx H2', id: 'md-h-2' },
    ]);
  });

  it('treats `---` after a blank line as a thematic break, not setext', () => {
    const src = ['para', '', '---', '', '# after'].join('\n');
    expect(extractHeadings(src).map((h) => h.text)).toEqual(['after']);
  });

  it('does not treat a fenced code close (```) as a setext underline', () => {
    const src = ['prior line', '```', 'body', '```', '', '# real'].join('\n');
    expect(extractHeadings(src).map((h) => h.text)).toEqual(['real']);
  });

  it('accepts 0–3 leading spaces before ATX hashes (CommonMark 4.2)', () => {
    expect(extractHeadings('   # indented').map((h) => h.text)).toEqual([
      'indented',
    ]);
  });

  it('handles CRLF line endings', () => {
    const src = '# One\r\n## Two\r\n';
    expect(extractHeadings(src).map((h) => h.text)).toEqual(['One', 'Two']);
  });
});

describe('MarkdownToc', () => {
  const headings: TocHeading[] = [
    { level: 1, text: 'Alpha', id: 'md-h-0' },
    { level: 2, text: 'Beta', id: 'md-h-1' },
    { level: 3, text: 'Gamma', id: 'md-h-2' },
  ];

  it('renders an entry per heading with data-level for indent', () => {
    const { container } = render(
      <MarkdownToc
        headings={headings}
        activeId={null}
        collapsed={false}
        onToggleCollapsed={() => {}}
        onSelect={() => {}}
      />,
    );
    const entries = container.querySelectorAll('.file-viewer-md-toc-entry');
    expect(entries).toHaveLength(3);
    expect(entries[0]).toHaveAttribute('data-level', '1');
    expect(entries[1]).toHaveAttribute('data-level', '2');
    expect(entries[2]).toHaveAttribute('data-level', '3');
    expect(entries[0]).toHaveTextContent('Alpha');
  });

  it('marks the active entry when activeId matches', () => {
    const { container } = render(
      <MarkdownToc
        headings={headings}
        activeId="md-h-1"
        collapsed={false}
        onToggleCollapsed={() => {}}
        onSelect={() => {}}
      />,
    );
    const entries = container.querySelectorAll('.file-viewer-md-toc-entry');
    expect(entries[0].classList.contains('active')).toBe(false);
    expect(entries[1].classList.contains('active')).toBe(true);
    expect(entries[2].classList.contains('active')).toBe(false);
  });

  it('fires onSelect with the clicked heading', () => {
    const onSelect = vi.fn();
    const { container } = render(
      <MarkdownToc
        headings={headings}
        activeId={null}
        collapsed={false}
        onToggleCollapsed={() => {}}
        onSelect={onSelect}
      />,
    );
    const entries = container.querySelectorAll('.file-viewer-md-toc-entry');
    fireEvent.click(entries[2]);
    expect(onSelect).toHaveBeenCalledTimes(1);
    expect(onSelect).toHaveBeenCalledWith(headings[2]);
  });

  it('hides the entry list when collapsed and shows only the expand button', () => {
    const onToggle = vi.fn();
    const { container, getByLabelText, queryByLabelText } = render(
      <MarkdownToc
        headings={headings}
        activeId={null}
        collapsed
        onToggleCollapsed={onToggle}
        onSelect={() => {}}
      />,
    );
    expect(container.querySelector('.file-viewer-md-toc-list')).toBeNull();
    expect(queryByLabelText('Outline')).not.toBeNull();
    fireEvent.click(getByLabelText('Expand outline'));
    expect(onToggle).toHaveBeenCalledTimes(1);
  });

  it('renders a fallback message when there are no headings', () => {
    const { container } = render(
      <MarkdownToc
        headings={[]}
        activeId={null}
        collapsed={false}
        onToggleCollapsed={() => {}}
        onSelect={() => {}}
      />,
    );
    expect(container.querySelector('.file-viewer-md-toc-empty')).not.toBeNull();
    expect(container.querySelector('.file-viewer-md-toc-entry')).toBeNull();
  });
});
