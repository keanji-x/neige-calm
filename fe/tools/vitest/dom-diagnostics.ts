/* Appends a bounded a11y report to Testing Library query failures, AFTER the dump so it survives the
   mutation runner's head+tail truncation. A diagnostic, not a gate: it only appends to an error already being thrown. */

// Only the DOM-bearing projects get this; the import is dynamic so `platform-independent` never resolves it.
if (typeof document !== 'undefined') {
  const { configure, getConfig } = await import('@testing-library/react');

  /* A heuristic, not the implicit-role mapping: treating every element as queryable would list every
   * `aria-hidden` icon in the app and bury the answer. */
  const MEANINGFUL = [
    'button', 'a[href]', 'input', 'select', 'textarea', 'summary', 'label',
    '[role]', '[tabindex]',
    'h1', 'h2', 'h3', 'h4', 'h5', 'h6',
    'img[alt]:not([alt=""])', 'table', 'ul', 'ol', 'dl', 'form', 'fieldset', 'dialog',
    'nav', 'main', 'header', 'footer', 'article', 'aside', 'section[aria-label]',
  ].join(', ');
  /** The three hiding *attributes*. CSS hiding is handled separately; see `cssHiddenRoots`. */
  const HIDING_ATTRIBUTES = '[inert], [aria-hidden="true"], [hidden]';
  const BODY_CHILD_LIMIT = 8;
  const HIDDEN_SUBTREE_LIMIT = 8;
  const QUERYABLE_LIMIT = 400;
  const CLASS_CHARS = 60;
  /** Every report line carries this, so a re-wrapped message is recognisable. */
  const MARKER = '[nc-a11y]';

  const identify = (element: Element): string => {
    const tag = element.tagName.toLowerCase();
    // Collapsed: a class value may contain a newline, and `REPORT_TAIL` recognises a previous report by its single-line indented shape.
    const raw = (element.getAttribute('class') ?? '').replace(/\s+/g, ' ').trim();
    if (raw === '') return `<${tag}>`;
    const shown = raw.length > CLASS_CHARS ? `${raw.slice(0, CLASS_CHARS)}…(+${raw.length - CLASS_CHARS} chars)` : raw;
    return `<${tag} class="${shown}">`;
  };

  /* The attribute reasons a selector can find; CSS-hidden subtrees are found by `cssHiddenRoots`, and both lists are reported. */
  const hiddenBecause = (element: Element): string => {
    const reasons: string[] = [];
    if (element.hasAttribute('inert')) reasons.push('inert');
    if (element.getAttribute('aria-hidden') === 'true') reasons.push('aria-hidden');
    if (element.hasAttribute('hidden')) reasons.push('hidden');
    const style = element.ownerDocument.defaultView?.getComputedStyle(element);
    if (style?.display === 'none') reasons.push('display:none');
    else if (style?.visibility === 'hidden') reasons.push('visibility:hidden');
    return reasons.join('+');
  };

  /* `display: none` does not reach a descendant's computed value, so it is found by walking ancestors;
   * `visibility: hidden` inherits but is overridable, so it is read off the element itself. */
  const displayNone = (style: CSSStyleDeclaration | undefined): boolean => style?.display === 'none';
  const visibilityHidden = (style: CSSStyleDeclaration | undefined): boolean => style?.visibility === 'hidden';

  /*
   * The CSS-hidden roots that hold something queryable. Only queryable elements start an ancestor walk
   * and every element tested is memoised; the HIGHEST hidden ancestor wins. Blind spot: shadow roots
   * (`querySelectorAll` does not pierce them), so a hidden subtree inside one reports "none".
   */
  const cssHiddenRoots = (body: HTMLElement): { roots: Map<Element, number>; examined: number; total: number } => {
    const queryable = Array.from(body.querySelectorAll(MEANINGFUL));
    const examined = queryable.slice(0, QUERYABLE_LIMIT);
    const memo = new Map<Element, CSSStyleDeclaration | undefined>();
    const styleOf = (element: Element): CSSStyleDeclaration | undefined => {
      if (!memo.has(element)) memo.set(element, element.ownerDocument.defaultView?.getComputedStyle(element));
      return memo.get(element);
    };
    const roots = new Map<Element, number>();
    for (const element of examined) {
      let root: Element | null = null;
      // The walk runs to `null` so a hidden `body` is itself a candidate root.
      for (let node: Element | null = element; node !== null; node = node.parentElement) {
        if (displayNone(styleOf(node))) root = node;
      }
      // Only if no `display:none` ancestor explains it: the highest CONTIGUOUSLY hidden ancestor, so an
      // override back to `visible` lower down stops the climb.
      if (root === null && visibilityHidden(styleOf(element))) {
        root = element;
        for (let node = element.parentElement; node !== null && visibilityHidden(styleOf(node)); node = node.parentElement) {
          root = node;
        }
      }
      if (root === null) continue;
      roots.set(root, (roots.get(root) ?? 0) + 1);
    }
    return { roots, examined: examined.length, total: queryable.length };
  };

  /** Writes to `error.message` if that is possible at all, and otherwise does nothing. */
  const append = (error: Error, text: string): void => {
    try {
      error.message = `${error.message}\n\n${text}`;
    } catch {
      /* Frozen or read-only error: leave it exactly as Testing Library made it. */
    }
  };

  const capped = <T>(items: readonly T[], limit: number, render: (item: T) => string): string => {
    const shown = items.slice(0, limit).map(render);
    if (items.length > limit) shown.push(`… and ${items.length - limit} more, not shown`);
    return shown.map((line) => `\n  ${line}`).join('');
  };

  const report = (container: Container): string => {
    // `container` is only used to reach its document; a detached element still has the real `ownerDocument.body`.
    const ownerDocument = container.ownerDocument;
    const { body } = ownerDocument;
    const children = Array.from(body.children);

    /* Body's direct children answer "is the portal there at all?". */
    const inventory = capped(children, BODY_CHILD_LIMIT, (child) => {
      const why = hiddenBecause(child);
      return `${identify(child)}${why === '' ? '' : ` — ${why}`}`;
    });

    // Decoration is `aria-hidden` everywhere in this app. `matches` first: `querySelector` excludes the
    // root, so a hidden element that is itself the queryable one would otherwise read as "none".
    const holdsQueryable = (element: Element): boolean =>
      element.matches(MEANINGFUL) || element.querySelector(MEANINGFUL) !== null;
    /* Scanned from the DOCUMENT: `querySelectorAll` never returns the node it is called on, so scanning
     * `body` would exclude `<body>` itself and `<html>` above it. */
    const byAttribute = Array.from(ownerDocument.querySelectorAll(HIDING_ATTRIBUTES)).filter(holdsQueryable);
    const { roots, examined, total } = cssHiddenRoots(body);
    // A CSS-hidden root that also carries a hiding attribute is one subtree,
    // not two; the attribute list already names it.
    const byStyle = Array.from(roots.entries()).filter(([element]) => !byAttribute.includes(element));
    const subtrees = [
      ...byAttribute.map((element) => ({ element, held: null as number | null })),
      ...byStyle.map(([element, held]) => ({ element, held })),
    ];
    const hidden = subtrees.length === 0
      ? '\n  none'
      : capped(subtrees, HIDDEN_SUBTREE_LIMIT, ({ element, held }) =>
        `${identify(element)} — ${hiddenBecause(element)}${held === null ? '' : ` (holds ${held} queryable)`}`);
    // The queryable scan is the only unbounded walk here, so when it is capped
    // the report says so rather than implying the CSS list is complete.
    const scanned = examined === total ? '' : `\n  [only the first ${examined} of ${total} queryable elements were`
      + ' scanned for CSS hiding; the list above may be incomplete]';

    return [
      `${MARKER} document.body children (${children.length}):${inventory}`,
      `${MARKER} subtrees out of the accessibility tree that hold queryable elements `
        + `(${subtrees.length}, by inert/aria-hidden/hidden/display/visibility):${hidden}${scanned}`,
    ].join('\n');
  };

  const inherited = getConfig().getElementError;
  type Container = Parameters<typeof inherited>[1];

  /* Installing twice would wrap the wrapper and print the report twice; the mark keeps that true even
   * if a project turns `isolate` off. */
  const INSTALLED = Symbol.for('nc.a11y-diagnostics.installed');
  if (!(INSTALLED in inherited)) {
    /* On timeout `waitFor` re-wraps via `getElementError(lastError.message, …)` and appends a second
     * `prettyDOM` dump after it, so the previous report is stripped and a fresh one appended: it must be
     * MOVED to the end, not skipped, or head+tail truncation keeps the dump and drops the report. */
    /* Matched by SHAPE, to the end of the string: a message may legitimately contain the prefix (a
     * query's own search string is printed back), and a greedy `$` anchor still matches from a mid-string
     * occurrence. A message deliberately ending in a byte-identical report block is still stripped; accepted. */
    const escaped = MARKER.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
    /** The report's own body lines: two-space indented, nothing else. */
    const INDENTED = '(?:\\n {2}[^\\n]*)*';
    const REPORT_TAIL = new RegExp(
      `\\n\\n(?:${escaped} document\\.body children \\(\\d+\\):${INDENTED}`
      + `\\n${escaped} subtrees out of the accessibility tree[^\\n]*:${INDENTED}`
      + `|${escaped} unavailable: [^\\n]*)$`,
    );
    const withoutReport = (text: string): string => text.replace(REPORT_TAIL, '');

    /* The expensive half, split out so `withReport` can defer it: `inherited` is a `prettyDOM`, and
     * `report` adds a `getComputedStyle` per queryable element. */
    const enrich = (message: string | null, container: Container) => {
      /*
       * `typeof message !== 'string'`, not `=== null`. `null` is Testing Library rendering one element
       * on its way to "Found multiple elements" — a report there lands between the dumps. `undefined`
       * arrives when a `waitFor` callback throws a non-Error; `undefined.indexOf` inside `onTimeout`'s
       * `setTimeout` hung the promise instead of failing it.
       */
      if (typeof message !== 'string') return inherited(message, container);
      const error = inherited(withoutReport(message), container);
      /*
       * A diagnostic that throws would replace a real failure with a useless one. The second `append`
       * goes through the same guard: a frozen error rejects the failure notice for the same reason it
       * rejected the report. `String(cause)` is itself fallible (`Symbol.toPrimitive` can throw).
       */
      try {
        append(error, report(container));
      } catch (cause) {
        let described: string;
        try {
          described = String(cause);
        } catch {
          described = 'a cause that could not be converted to a string';
        }
        // Single-line for the same reason `identify` collapses whitespace.
        append(error, `${MARKER} unavailable: ${described.replace(/\s+/g, ' ')}`);
      }
      return error;
    };

    /**
     * Is this call a `waitFor` poll? `wait-for.js` sets `_disableExpensiveErrorDiagnostics` for the
     * synchronous extent of the callback and nothing else does; the timeout re-wrap runs outside it. A
     * private field, hence the cast; if it disappears every call becomes eager — slow, not wrong. An
     * async callback is misread as synchronous (the flag is restored before the promise rejects).
     */
    const polling = (): boolean =>
      (getConfig() as unknown as { _disableExpensiveErrorDiagnostics?: boolean })
        ._disableExpensiveErrorDiagnostics === true;

    /* A failing poll builds dump + report lazily on the first `.message` read (`wait-for.js` keeps only the last poll's error);
       a non-poll failure is built EAGERLY because `try { … } finally { app.dispose() }` tears the DOM down before Vitest reads `.message`. */
    const withReport = (message: string | null, container: Container) => {
      if (typeof message !== 'string') return enrich(message, container);
      if (!polling()) return enrich(message, container);
      let built: Error | undefined;
      const resolved = (): Error => {
        if (built === undefined) {
          try {
            built = enrich(message, container);
          } catch {
            // A producer underneath that throws must not turn a query failure into a throw inside a `.message` read.
            built = new Error(message);
          }
        }
        return built;
      };
      const error = new Error(message);
      const defer = (key: 'message' | 'name'): void => {
        let override: string | undefined;
        Object.defineProperty(error, key, {
          configurable: true,
          enumerable: false,
          get: () => override ?? resolved()[key],
          set: (next: string) => { override = next; },
        });
      };
      defer('message');
      defer('name');
      return error;
    };
    Object.defineProperty(withReport, INSTALLED, { value: true });
    configure({ getElementError: withReport });
  }
}

export {};
