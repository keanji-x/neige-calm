import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  type HTMLAttributes,
  type KeyboardEvent as ReactKeyboardEvent,
} from 'react';
import ReactMarkdown, { type Components } from 'react-markdown';
import remarkGfm from 'remark-gfm';
import {
  extractHeadings,
  type TocHeading,
  type TocLevel,
} from './file-viewer-markdown-toc';

/**
 * Adapter surface every pane implements so the shared SearchBar can drive
 * "next / prev / total" against pane-native internals without knowing what
 * they are. CodePane goes through @codemirror/search; MarkdownPane goes
 * through the CSS Custom Highlight API.
 */
export interface PaneSearchAdapter {
  setQuery(pattern: string): void;
  next(): void;
  prev(): void;
  dispose(): void;
}

export interface MarkdownPaneProps {
  path: string;
  text: string;
  /**
   * Handshake: MarkdownPane calls this once after the preview mounts with a
   * search adapter tied to its DOM tree; and again with `null` on unmount /
   * before rebuilding for a different file.
   */
  onSearchAdapterReady?: (adapter: PaneSearchAdapter | null) => void;
  /** Fired whenever match count / current index changes. */
  onSearchCount?: (current: number, total: number) => void;
  /**
   * Keydown handler for `/`. The pane owns capturing the key because the
   * container needs `tabIndex=0` and its own listener — using CodeMirror-
   * style keymap here would require pulling CM into the markdown bundle.
   */
  onSlashOpen?: () => void;
  /** TOC integration: emitted once per source-text change. */
  onHeadingsChange?: (headings: TocHeading[]) => void;
  /** Scrollspy: emits the DOM-order-first heading currently in the top zone. */
  onActiveHeadingChange?: (id: string | null) => void;
  /**
   * Notified with the pane's scroll container element (or `null` on unmount).
   * The caller uses this to scope TOC-click heading lookups to THIS pane —
   * heading ids like `md-h-N` are not unique across multiple file-viewer
   * cards mounted in the same wave, so `document.getElementById` would land
   * on the first card's heading and misdirect the scroll.
   */
  onContainerRef?: (el: HTMLElement | null) => void;
}

// CSS Custom Highlight API guard. Stable in Chrome/Safari/Firefox (2025);
// unavailable environments (older evergreen, some jsdom versions) fall
// through to a no-op adapter — the search bar still opens and accepts
// input, just no highlighting.
type HighlightsRegistry = {
  set(name: string, value: unknown): unknown;
  delete(name: string): boolean;
};

interface CssWithHighlights {
  highlights?: HighlightsRegistry;
}

function highlightsRegistry(): HighlightsRegistry | null {
  if (typeof CSS === 'undefined') return null;
  const reg = (CSS as unknown as CssWithHighlights).highlights;
  return reg ?? null;
}

interface HighlightCtor {
  new (...ranges: Range[]): unknown;
}

function highlightCtor(): HighlightCtor | null {
  const g = globalThis as { Highlight?: HighlightCtor };
  return g.Highlight ?? null;
}

const HIGHLIGHT_ALL = 'fv-search-all';
const HIGHLIGHT_CURRENT = 'fv-search-current';

let hasWarnedNoHighlight = false;

/**
 * Walk the pane subtree, collect case-insensitive substring matches of
 * `needle` as DOM Ranges. Skips <script> and <style> nodes.
 */
export function collectMatches(root: Element, needle: string): Range[] {
  if (!needle) return [];
  const lower = needle.toLowerCase();
  const ranges: Range[] = [];
  const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT, {
    acceptNode(node) {
      const parent = node.parentNode as Element | null;
      if (!parent) return NodeFilter.FILTER_REJECT;
      const tag = parent.nodeName;
      if (tag === 'SCRIPT' || tag === 'STYLE') return NodeFilter.FILTER_REJECT;
      return NodeFilter.FILTER_ACCEPT;
    },
  });
  let node: Node | null;
  while ((node = walker.nextNode())) {
    const text = node.nodeValue ?? '';
    if (!text) continue;
    const lowered = text.toLowerCase();
    let from = 0;
    while (from <= lowered.length - lower.length) {
      const idx = lowered.indexOf(lower, from);
      if (idx === -1) break;
      const range = document.createRange();
      range.setStart(node, idx);
      range.setEnd(node, idx + lower.length);
      ranges.push(range);
      from = idx + lower.length;
    }
  }
  return ranges;
}

/**
 * Build a PaneSearchAdapter over a DOM subtree.
 * `onCount` fires whenever match count / current index changes so the bar
 * can render `3/17`.
 */
export function createMarkdownSearchAdapter(
  container: Element,
  onCount: (current: number, total: number) => void,
): PaneSearchAdapter {
  const registry = highlightsRegistry();
  const Highlight = highlightCtor();
  if (!registry || !Highlight) {
    if (
      !hasWarnedNoHighlight &&
      typeof process !== 'undefined' &&
      process.env?.NODE_ENV !== 'production'
    ) {
      hasWarnedNoHighlight = true;
      // eslint-disable-next-line no-console
      console.warn(
        '[file-viewer] CSS Custom Highlight API unavailable; markdown search will render without highlights.',
      );
    }
    return {
      setQuery: () => onCount(0, 0),
      next: () => {},
      prev: () => {},
      dispose: () => {},
    };
  }
  let ranges: Range[] = [];
  let currentIndex = 0;
  const applyHighlights = () => {
    if (ranges.length === 0) {
      registry.delete(HIGHLIGHT_ALL);
      registry.delete(HIGHLIGHT_CURRENT);
      onCount(0, 0);
      return;
    }
    registry.set(HIGHLIGHT_ALL, new Highlight(...ranges));
    const cur = ranges[currentIndex];
    if (cur) {
      registry.set(HIGHLIGHT_CURRENT, new Highlight(cur));
      cur.startContainer.parentElement?.scrollIntoView({
        block: 'nearest',
        inline: 'nearest',
      });
    } else {
      registry.delete(HIGHLIGHT_CURRENT);
    }
    onCount(currentIndex + 1, ranges.length);
  };
  return {
    setQuery(pattern) {
      ranges = collectMatches(container, pattern);
      currentIndex = 0;
      applyHighlights();
    },
    next() {
      if (ranges.length === 0) return;
      currentIndex = (currentIndex + 1) % ranges.length;
      applyHighlights();
    },
    prev() {
      if (ranges.length === 0) return;
      currentIndex = (currentIndex - 1 + ranges.length) % ranges.length;
      applyHighlights();
    },
    dispose() {
      ranges = [];
      currentIndex = 0;
      registry.delete(HIGHLIGHT_ALL);
      registry.delete(HIGHLIGHT_CURRENT);
    },
  };
}

export function MarkdownPane({
  path,
  text,
  onSearchAdapterReady,
  onSearchCount,
  onSlashOpen,
  onHeadingsChange,
  onActiveHeadingChange,
  onContainerRef,
}: MarkdownPaneProps) {
  const containerRef = useRef<HTMLDivElement | null>(null);
  const setContainerRef = useCallback(
    (el: HTMLDivElement | null) => {
      containerRef.current = el;
      onContainerRef?.(el);
    },
    [onContainerRef],
  );
  const onSearchCountRef = useRef(onSearchCount);
  const onSlashOpenRef = useRef(onSlashOpen);
  const onSearchAdapterReadyRef = useRef(onSearchAdapterReady);
  useEffect(() => {
    onSearchCountRef.current = onSearchCount;
  }, [onSearchCount]);
  useEffect(() => {
    onSlashOpenRef.current = onSlashOpen;
  }, [onSlashOpen]);
  useEffect(() => {
    onSearchAdapterReadyRef.current = onSearchAdapterReady;
  }, [onSearchAdapterReady]);

  const headings = useMemo(() => extractHeadings(text), [text]);

  // Renders execute the component body top-down; resetting the counter here,
  // before ReactMarkdown descends into its children, keeps the h1–h4 renderer
  // in lockstep with `headings` (both derived from the same source text in
  // document order).
  const counterRef = useRef(0);
  counterRef.current = 0;

  const components = useMemo<Components>(() => {
    function makeRenderer(level: TocLevel) {
      const Tag = `h${level}` as const;
      function HeadingRenderer({
        children,
        node: _node,
        ...rest
      }: HTMLAttributes<HTMLHeadingElement> & { node?: unknown }) {
        const n = counterRef.current++;
        const h = headings[n];
        const id = h ? h.id : undefined;
        return (
          <Tag id={id} {...rest}>
            {children}
          </Tag>
        );
      }
      HeadingRenderer.displayName = `MarkdownH${level}`;
      return HeadingRenderer;
    }
    return {
      h1: makeRenderer(1),
      h2: makeRenderer(2),
      h3: makeRenderer(3),
      h4: makeRenderer(4),
    };
  }, [headings]);

  useEffect(() => {
    onHeadingsChange?.(headings);
  }, [headings, onHeadingsChange]);

  useEffect(() => {
    const container = containerRef.current;
    if (
      !container ||
      headings.length === 0 ||
      typeof IntersectionObserver === 'undefined'
    ) {
      onActiveHeadingChange?.(null);
      return;
    }
    const targets: HTMLElement[] = [];
    for (const h of headings) {
      // Attribute selector matches unambiguously in every engine; some
      // (notably jsdom) mis-resolve `#dashed-digit-ids` in Element-scoped
      // querySelector.
      const el = container.querySelector<HTMLElement>(`[id="${h.id}"]`);
      if (el) targets.push(el);
    }
    if (targets.length === 0) {
      onActiveHeadingChange?.(null);
      return;
    }
    const visibility = new Map<string, boolean>();
    const observer = new IntersectionObserver(
      (entries) => {
        for (const e of entries) {
          visibility.set(e.target.id, e.isIntersecting);
        }
        let firstActive: string | null = null;
        for (const h of headings) {
          if (visibility.get(h.id)) {
            firstActive = h.id;
            break;
          }
        }
        onActiveHeadingChange?.(firstActive);
      },
      { root: container, rootMargin: '0px 0px -70% 0px', threshold: 0 },
    );
    for (const t of targets) observer.observe(t);
    return () => observer.disconnect();
    // headings identity changes only when text changes (useMemo above), so
    // this effect recomputes exactly when the rendered heading DOM does.
  }, [headings, onActiveHeadingChange]);

  // Build a fresh adapter whenever the file identity changes. The path/text
  // deps ensure that swapping to a new file disposes the previous adapter
  // and clears any lingering highlights.
  useEffect(() => {
    const el = containerRef.current;
    if (!el) return;
    const adapter = createMarkdownSearchAdapter(el, (cur, total) => {
      onSearchCountRef.current?.(cur, total);
    });
    onSearchAdapterReadyRef.current?.(adapter);
    return () => {
      adapter.dispose();
      onSearchAdapterReadyRef.current?.(null);
    };
  }, [path, text]);

  // Keydown for `/` — only open the bar when the target is NOT an editable
  // element (so a search-bar <input> above us can still type "/"), and no
  // modifier keys are held.
  const onKeyDown = (e: ReactKeyboardEvent<HTMLDivElement>) => {
    if (e.key !== '/' || e.metaKey || e.ctrlKey || e.altKey) return;
    const target = e.target as HTMLElement | null;
    const tag = target?.tagName;
    if (
      target?.isContentEditable ||
      tag === 'INPUT' ||
      tag === 'TEXTAREA' ||
      tag === 'SELECT'
    ) {
      return;
    }
    e.preventDefault();
    onSlashOpenRef.current?.();
  };

  // The container needs tabIndex + keydown so `/` opens the search bar
  // without triggering Firefox's quick-find. Both a11y rules below are
  // deliberately silenced: the element is a content region, not
  // interactive controls — the keydown is a page-level shortcut hook.
  return (
    /* eslint-disable jsx-a11y/no-noninteractive-element-interactions,
                      jsx-a11y/no-noninteractive-tabindex */
    <div
      ref={setContainerRef}
      className="file-viewer-markdown-body calm-prose"
      data-wheel-pane="markdown"
      data-path={path}
      role="region"
      aria-label="Markdown preview"
      tabIndex={0}
      onKeyDown={onKeyDown}
    >
      <ReactMarkdown remarkPlugins={[remarkGfm]} components={components}>
        {text}
      </ReactMarkdown>
    </div>
    /* eslint-enable jsx-a11y/no-noninteractive-element-interactions,
                     jsx-a11y/no-noninteractive-tabindex */
  );
}
