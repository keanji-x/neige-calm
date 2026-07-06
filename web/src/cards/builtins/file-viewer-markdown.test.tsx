// MarkdownPane tests — pin the react-markdown v10 safety contract so future
// changes (adding rehype-raw, custom urlTransform, etc.) can't silently
// broaden the XSS surface. The file-viewer intentionally ships without
// rehype-sanitize because react-markdown v10 defaults do not emit raw HTML
// nodes and neutralize `javascript:` URLs; these tests verify both.

import { describe, it, expect } from 'vitest';
import { render } from '@testing-library/react';
import { MarkdownPane } from './file-viewer-markdown';

describe('MarkdownPane XSS contract (react-markdown v10 defaults)', () => {
  it('renders raw <script> as literal text, not as a DOM element', () => {
    const src = 'before <script>window.__pwned = true;</script> after';
    const { container } = render(
      <MarkdownPane path="/tmp/x.md" text={src} />,
    );
    // No <script> node injected.
    expect(container.querySelector('script')).toBeNull();
    // The literal characters survive somewhere in the rendered text.
    expect(container.textContent ?? '').toContain('window.__pwned');
  });

  it('renders raw <img onerror> as literal text, not as an image', () => {
    const src = 'x <img src=x onerror="alert(1)"> y';
    const { container } = render(
      <MarkdownPane path="/tmp/x.md" text={src} />,
    );
    // No <img> node from the raw HTML.
    expect(container.querySelector('img')).toBeNull();
  });

  it('strips `javascript:` from markdown link hrefs', () => {
    const src = '[click me](javascript:alert(1))';
    const { container } = render(
      <MarkdownPane path="/tmp/x.md" text={src} />,
    );
    const anchor = container.querySelector('a');
    // react-markdown v10 defaultUrlTransform drops javascript: URLs — the
    // anchor either has no href or has an empty href, never the raw scheme.
    if (anchor) {
      expect(anchor.getAttribute('href')?.startsWith('javascript:')).not.toBe(
        true,
      );
    }
  });

  it('renders GFM tables into a real <table>', () => {
    const src = [
      '| a | b |',
      '| - | - |',
      '| 1 | 2 |',
    ].join('\n');
    const { container } = render(
      <MarkdownPane path="/tmp/x.md" text={src} />,
    );
    expect(container.querySelector('table')).not.toBeNull();
    expect(container.querySelectorAll('td')).toHaveLength(2);
  });
});
