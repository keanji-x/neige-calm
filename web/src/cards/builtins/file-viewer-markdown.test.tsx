// MarkdownPane tests — pin the react-markdown v10 safety contract so future
// changes (adding rehype-raw, custom urlTransform, etc.) can't silently
// broaden the XSS surface. The file-viewer intentionally ships without
// rehype-sanitize because react-markdown v10 defaults do not emit raw HTML
// nodes and neutralize `javascript:` URLs; these tests verify both.

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { act, render, waitFor } from '@testing-library/react';
import { MarkdownPane } from './file-viewer-markdown';
import type { TocHeading } from './file-viewer-markdown-toc';

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

// IntersectionObserver isn't implemented by jsdom. Capture the last-registered
// instance so tests can drive it directly, and expose a way to fire entries.
type IOCallback = ConstructorParameters<typeof IntersectionObserver>[0];
interface IOHandle {
  callback: IOCallback;
  observed: Element[];
  disconnected: boolean;
}
const ioHandles: IOHandle[] = [];

class MockIntersectionObserver {
  callback: IOCallback;
  observed: Element[] = [];
  constructor(cb: IOCallback) {
    this.callback = cb;
    ioHandles.push(this as unknown as IOHandle);
  }
  observe(el: Element) {
    this.observed.push(el);
  }
  unobserve() {}
  disconnect() {
    (this as unknown as IOHandle).disconnected = true;
  }
  takeRecords(): IntersectionObserverEntry[] {
    return [];
  }
}

describe('MarkdownPane heading id assignment', () => {
  it('emits headings via onHeadingsChange in document order', async () => {
    const onHeadingsChange = vi.fn();
    render(
      <MarkdownPane
        path="/tmp/x.md"
        text={'# One\n## Two\n### Three\n#### Four\n'}
        onHeadingsChange={onHeadingsChange}
      />,
    );
    await waitFor(() => expect(onHeadingsChange).toHaveBeenCalled());
    const last = onHeadingsChange.mock.calls.at(-1)![0] as TocHeading[];
    expect(last.map((h) => `${h.level}:${h.text}:${h.id}`)).toEqual([
      '1:One:md-h-0',
      '2:Two:md-h-1',
      '3:Three:md-h-2',
      '4:Four:md-h-3',
    ]);
  });

  it('assigns md-h-N ids to rendered h1–h4 in document order', () => {
    const { container } = render(
      <MarkdownPane
        path="/tmp/x.md"
        text={'# One\n## Two\n### Three\n#### Four\n'}
      />,
    );
    const h1 = container.querySelector('h1');
    const h2 = container.querySelector('h2');
    const h3 = container.querySelector('h3');
    const h4 = container.querySelector('h4');
    expect(h1?.getAttribute('id')).toBe('md-h-0');
    expect(h2?.getAttribute('id')).toBe('md-h-1');
    expect(h3?.getAttribute('id')).toBe('md-h-2');
    expect(h4?.getAttribute('id')).toBe('md-h-3');
  });
});

describe('MarkdownPane scrollspy', () => {
  const OriginalIO = globalThis.IntersectionObserver;

  beforeEach(() => {
    ioHandles.length = 0;
    (globalThis as unknown as {
      IntersectionObserver: typeof IntersectionObserver;
    }).IntersectionObserver = MockIntersectionObserver as unknown as typeof IntersectionObserver;
  });

  afterEach(() => {
    (globalThis as unknown as {
      IntersectionObserver: typeof IntersectionObserver;
    }).IntersectionObserver = OriginalIO;
  });

  it('reports the first intersecting heading as active, in document order', async () => {
    const onActive = vi.fn();
    render(
      <MarkdownPane
        path="/tmp/x.md"
        text={'# One\n## Two\n### Three\n'}
        onActiveHeadingChange={onActive}
      />,
    );
    await waitFor(() => expect(ioHandles.length).toBeGreaterThan(0));
    const io = ioHandles[ioHandles.length - 1];
    const [h1, h2, h3] = ['md-h-0', 'md-h-1', 'md-h-2'].map(
      (id) => document.getElementById(id)!,
    );
    // Two intersecting: the DOM-first one wins.
    act(() => {
      io.callback(
        [
          { target: h1, isIntersecting: false } as unknown as IntersectionObserverEntry,
          { target: h2, isIntersecting: true } as unknown as IntersectionObserverEntry,
          { target: h3, isIntersecting: true } as unknown as IntersectionObserverEntry,
        ],
        io as unknown as IntersectionObserver,
      );
    });
    expect(onActive).toHaveBeenLastCalledWith('md-h-1');

    // Then h1 also intersects — it now becomes active (first in DOM order).
    act(() => {
      io.callback(
        [{ target: h1, isIntersecting: true } as unknown as IntersectionObserverEntry],
        io as unknown as IntersectionObserver,
      );
    });
    expect(onActive).toHaveBeenLastCalledWith('md-h-0');

    // Nothing intersecting → null.
    act(() => {
      io.callback(
        [
          { target: h1, isIntersecting: false } as unknown as IntersectionObserverEntry,
          { target: h2, isIntersecting: false } as unknown as IntersectionObserverEntry,
          { target: h3, isIntersecting: false } as unknown as IntersectionObserverEntry,
        ],
        io as unknown as IntersectionObserver,
      );
    });
    expect(onActive).toHaveBeenLastCalledWith(null);
  });
});
