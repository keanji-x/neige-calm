/*
 * Why this file exists — #1161.
 *
 * A `findByRole` that times out tells you almost nothing about *why*, and the
 * two channels that would tell you are both closed by construction:
 *
 *  1. **The roles list is unreachable.** `findBy*` is `waitFor(getBy*)`, and
 *     `waitFor` runs the getter inside `runWithExpensiveErrorDiagnosticsDisabled`.
 *     `getMissingError` in `@testing-library/dom/queries/role.js` checks that
 *     flag *first* and returns the short branch — `Unable to find role="button"
 *     and name "…"` — with no "Here are the accessible roles:" section. The
 *     error that surfaces is the one thrown on the last poll, i.e. always the
 *     short one. No configuration reopens this.
 *
 *  2. **The DOM dump is cut before the interesting part.** What is left is
 *     `getElementError`'s `prettyDOM(container)`, capped at `DEBUG_PRINT_LIMIT`
 *     (7000 chars). A `screen` query's container is `document.body`, and a
 *     portalled overlay is body's *last* child — so on the shell tests the one
 *     subtree the failure is about is exactly the part the cap removes. The
 *     #1161 failure captured from CI carried a 7546-character message whose
 *     dump ended inside the sidebar, well before any overlay.
 *
 * So the question that actually discriminates — *was the element missing from
 * the DOM, or present but out of the accessibility tree?* — was unanswerable
 * from a CI failure. This appends a small, bounded report that answers it.
 *
 * It is appended **after** the DOM dump on purpose: `tools/mutation/runner.ts`
 * truncates captured failure messages head+tail, so anything at the end of the
 * message survives into the CI artifact even when the dump does not.
 *
 * This is a diagnostic, not a gate: it only ever appends to the message of an
 * error Testing Library was already going to throw. Two things were measured
 * rather than asserted, because "it cannot change a test outcome" is the kind
 * of claim that is only worth what was run against it:
 *
 *  - the whole suite is green with it installed. (No test count is quoted here
 *    on purpose: the first two drafts of this comment carried totals that were
 *    stale by the time they were read.) Query-failure messages *are* read — by
 *    this file's own tests, unavoidably, one of which asserts an exact message
 *    against a producer it installed itself — but nothing outside this file's
 *    own fixtures does; and
 *  - `getElementError` is called on *every* `waitFor` poll, not just the last
 *    one, so the cost matters. Measured on a 200-node body holding 100
 *    buttons: 5.10ms per call against 4.07ms without, so the report costs
 *    ~1.03ms — 25% on top of what `prettyDOM` already spends. Nearly all of it
 *    is the CSS scan's `getComputedStyle` per queryable element.
 *
 *    25% sounds worse than it is, and the absolute number is the one that
 *    decides: only *failing* DTL queries reach here, and the worst case is a
 *    `findBy*` that times out — 1000ms at a 50ms interval is 20 polls, so
 *    ~20ms added to a 1000ms budget. A `waitFor` over a plain `expect` never
 *    calls `getElementError` at all. This is a bound, not an impossibility: a
 *    `findBy*` whose element appears in the last ~20ms before its deadline
 *    could be pushed over by exactly this. Nothing cheaper was available that
 *    still answers the CSS question, and a query that close to its deadline is
 *    already a flake.
 *
 * What it *would* break is a test asserting the exact text of a query failure
 * — `toThrow(exactMessage)` or a snapshot, not `toThrow('Unable to find')`,
 * which still matches. None exists; one written later fails loudly, not
 * silently.
 */

// Only the DOM-bearing projects get this; `platform-independent` runs in node,
// where importing React Testing Library is both pointless and load-bearing on
// globals it does not have. The import is dynamic so the module is not even
// resolved there.
if (typeof document !== 'undefined') {
  const { configure, getConfig } = await import('@testing-library/react');

  /*
   * What counts as "something a query could have been looking for".
   *
   * **This is a heuristic, not the implicit-role mapping.** It covers the
   * interactive elements plus the headings, landmarks and images these suites
   * actually query, because the alternative — treating every element as
   * queryable — lists every `aria-hidden` icon in the app and buries the
   * answer. The cost is stated rather than hidden: a hidden subtree whose only
   * queryable content is an element outside this list reads as decoration and
   * is left out.
   */
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
    // Collapsed, because a class value may legally contain a newline and the
    // report's lines must stay single-line: `REPORT_TAIL` recognises a previous
    // report by its indented shape, and one stray newline made it unmatchable,
    // so a re-wrap appended a second report and stranded the first mid-message.
    const raw = (element.getAttribute('class') ?? '').replace(/\s+/g, ' ').trim();
    if (raw === '') return `<${tag}>`;
    const shown = raw.length > CLASS_CHARS ? `${raw.slice(0, CLASS_CHARS)}…(+${raw.length - CLASS_CHARS} chars)` : raw;
    return `<${tag} class="${shown}">`;
  };

  /*
   * Four things take a node out of the accessibility tree: `inert`,
   * `aria-hidden`, the `hidden` attribute, and CSS (`display:none` /
   * `visibility:hidden`). The first three are attributes and a selector can
   * find them. **The fourth is the reason this function is not the whole
   * story** — no selector expresses a computed style, so CSS-hidden subtrees
   * are found by `cssHiddenRoots` below instead, and both lists are reported.
   */
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

  /*
   * `display` and `visibility` need opposite treatment, and conflating them was
   * a bug in both directions.
   *
   * `display: none` does **not** reach a descendant's computed value, so it can
   * only be found by walking ancestors. `visibility: hidden` **does** inherit —
   * but it is also overridable, and `<div style="visibility:hidden"><button
   * style="visibility:visible">` is a visible button that an ancestor walk
   * would have called hidden. So visibility is read off the element itself,
   * where inheritance and any override are already resolved.
   */
  const displayNone = (style: CSSStyleDeclaration | undefined): boolean => style?.display === 'none';
  const visibilityHidden = (style: CSSStyleDeclaration | undefined): boolean => style?.visibility === 'hidden';

  /*
   * The CSS-hidden roots that hold something queryable.
   *
   * This exists because the attribute selector above is blind to exactly the
   * case #1161 needs: `ui/dialog/public.tsx:139` puts `display: none` on
   * `.dialog-body` whenever a child view is showing, and the `Create track`
   * button lives inside it. A report that answered "no hidden subtree holds
   * anything queryable" there would be *actively wrong* — the precise
   * misdiagnosis this file exists to prevent.
   *
   * **The element itself is not where the answer is.** `getComputedStyle` on a
   * button inside a `display: none` container still reports the button's own
   * `display` — the container's `none` is not inherited into the child's
   * computed value. So the ancestors have to be walked; asking only the element
   * finds nothing, which is how the first draft of this silently reported
   * "none" for the very case it was added for.
   *
   * Walking every element in the document would cost a `getComputedStyle` per
   * node. Only queryable elements start a walk, and every element tested along
   * the way is memoised, so a subtree with fifty hidden buttons pays for its
   * shared ancestors once. The *highest* hidden ancestor wins, because a reader
   * wants "this container went away", not each leaf under it.
   *
   * **Blind spot, stated rather than discovered later:** `querySelectorAll`
   * does not pierce shadow roots and `parentElement` stops at the shadow
   * boundary, so a hidden subtree inside an open shadow root is not found. No
   * component in this app renders into one today; if one starts to, this
   * reports "none" for it and that is a wrong answer, not a missing one.
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
      // `body` is included deliberately: a page whose own `body` is hidden read
      // as fully visible while the walk stopped one level short of it, which is
      // this file's own misdiagnosis class one level up.
      for (let node: Element | null = element; node !== null; node = node.parentElement) {
        if (displayNone(styleOf(node))) root = node;
      }
      // Only if no `display:none` ancestor explains it: the element's own
      // resolved visibility, then the highest *contiguously* hidden ancestor,
      // so an override back to `visible` lower down stops the climb.
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
    // `container` is only used to reach its document — a detached element still
    // has the real `ownerDocument.body`, so the inventory is the page's either
    // way. The caller turns any throw from here into a stated "unavailable"
    // rather than losing the underlying failure.
    const ownerDocument = container.ownerDocument;
    const { body } = ownerDocument;
    const children = Array.from(body.children);

    /*
     * Body's direct children answer "is the portal there at all?" — a dialog
     * that never mounted leaves one child (the render container); a dialog that
     * mounted and was hidden leaves two, and the second one says why.
     */
    const inventory = capped(children, BODY_CHILD_LIMIT, (child) => {
      const why = hiddenBecause(child);
      return `${identify(child)}${why === '' ? '' : ` — ${why}`}`;
    });

    // Decoration is `aria-hidden` everywhere in this app (every icon is). Only
    // a subtree that *contains* something queryable can explain a missing role.
    // `querySelector` excludes the root, so a hidden element that is itself
    // the queryable one — `<button aria-hidden="true">` — was reported as
    // "none": the report denying the very thing it was asked about.
    const holdsQueryable = (element: Element): boolean =>
      element.matches(MEANINGFUL) || element.querySelector(MEANINGFUL) !== null;
    /*
     * Scanned from the **document**, not from `body`.
     *
     * `querySelectorAll` never returns the node it is called on, so scanning
     * `body` silently excluded `<body>` itself and `<html>` above it. Rooting
     * the query at the document covers both plus every descendant in one
     * selector, which is why the `body.matches(…)` special case that used to
     * sit here is gone rather than joined by a sibling for `<html>`.
     *
     * The related bug in `cssHiddenRoots` — a `display:none` body reported as
     * nothing hidden — was *not* this: that scan is still rooted at `body`
     * below, and its blind spot was the ancestor walk stopping one level short,
     * fixed there by letting the walk run to `null`. Two different causes with
     * the same symptom, which is why the first was patched as a special case
     * and the shape only became clear at the third sighting.
     */
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

  /*
   * Installing twice would wrap the wrapper and print the report twice. That
   * cannot happen today only because each test file gets a fresh module
   * registry *and* a fresh Testing Library config — and #1123 proposes turning
   * `isolate` off for `web-dom`, which changes the first half of that. Marking
   * the installed function keeps the answer independent of which half holds.
   */
  const INSTALLED = Symbol.for('nc.a11y-diagnostics.installed');
  if (!(INSTALLED in inherited)) {
    /*
     * Testing Library re-wraps messages that already came from here, and the
     * worst offender is the path #1161 actually takes: on timeout `waitFor`
     * calls `getElementError(lastError.message, …)`, and the default
     * implementation appends a *second* `prettyDOM` dump after it. Simply
     * declining to re-append was the first attempt and it was wrong — measured,
     * the report then sat at offset 267 of a 594-character message with the
     * new dump running to the end, so the runner's head+tail truncation would
     * keep the dump and drop the report. The report has to be *moved*, not
     * skipped: strip the previous one off the incoming message and append a
     * fresh one, so it is last no matter how many times DTL re-wraps.
     *
     * The prefix is matched exactly, so a test whose own query text merely
     * contains the marker — `getByText('[nc-a11y] not present')` — is not
     * mistaken for a re-wrap and still gets its report.
     */
    /*
     * Matched by SHAPE, to the end of the string.
     *
     * The first version cut at the first occurrence of the prefix anywhere in
     * the message, which quietly deleted everything after it — and a message
     * may legitimately *contain* that text, because a query's own search string
     * ends up in `Unable to find an element with the text: …`. Testing
     * Library's trailing sentence then vanished.
     *
     * Anchoring to `$` was not enough either, and the test above caught it: a
     * greedy `[\s\S]*$` still matches starting from a mid-string occurrence. So
     * the whole tail has to look like the report — header line, indented body,
     * second header, indented body, end — which a decoy followed by ordinary
     * prose cannot satisfy.
     *
     * A message deliberately ending in a byte-identical report block would
     * still be stripped. That is accepted rather than defended: there is no
     * marker in a string that a string cannot also contain, and the error
     * object itself cannot be tracked because `waitFor` re-wraps by passing
     * `error.message`, not the error.
     */
    const escaped = MARKER.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
    /** The report's own body lines: two-space indented, nothing else. */
    const INDENTED = '(?:\\n {2}[^\\n]*)*';
    const REPORT_TAIL = new RegExp(
      `\\n\\n(?:${escaped} document\\.body children \\(\\d+\\):${INDENTED}`
      + `\\n${escaped} subtrees out of the accessibility tree[^\\n]*:${INDENTED}`
      + `|${escaped} unavailable: [^\\n]*)$`,
    );
    const withoutReport = (text: string): string => text.replace(REPORT_TAIL, '');

    const withReport = (message: string | null, container: Container) => {
      /*
       * `message === null` is Testing Library rendering *one element* on its way
       * to a larger error — `query-helpers.js` calls
       * `getElementError(null, element)` once per match to build the
       * "Found multiple elements" text. A report there is worse than useless:
       * it is not the final error, and the reports land *between* the element
       * dumps, so the strip below then cut the message at the first one and
       * took the rest with it. Measured on `getByText` with two matches: the
       * second element's dump and Testing Library's own "If this is
       * intentional, then use the `*AllBy*` variant" hint both disappeared —
       * this file making an existing message strictly worse. Declining here
       * leaves the strip only genuine re-wraps to act on.
       *
       * The test is `typeof message !== 'string'` and not `=== null` because
       * `undefined` reaches here too, and that path was far worse than a missing
       * report. `wait-for.js` calls `getElementError(error.message, …)` with
       * whatever the callback threw, so a `waitFor` whose callback throws a
       * non-Error — `throw { code: 1 }` — arrives with `message === undefined`.
       * `undefined.indexOf` then threw out of `onTimeout`, which runs inside a
       * `setTimeout`, so the `waitFor` promise **never settled** and the test
       * hung rather than failing. Testing Library's own implementation survives
       * it because `[message, dump].filter(Boolean)` simply drops `undefined`.
       */
      if (typeof message !== 'string') return inherited(message, container);
      const error = inherited(withoutReport(message), container);
      /*
       * A diagnostic that throws would replace a real failure with a useless
       * one, so every step here gives up quietly. Two things are load-bearing:
       * the *second* `append` is why this is a helper and not a bare try/catch
       * — if the message could not be written the first time (a frozen or
       * read-only error from some other `getElementError`) then writing the
       * failure notice would throw for exactly the same reason, and the
       * original error would be lost to a `TypeError`. And `String(cause)` is
       * itself fallible, since a thrown object can carry a `Symbol.toPrimitive`
       * that throws. When nothing can be written the error is returned
       * untouched, which is the outcome that loses the least.
       *
       * That inner guard is the one branch here with **no test behind it**, and
       * it is deliberate: `report()` only ever throws DOM exceptions and
       * `TypeError`s, all of which stringify, so nothing reachable through the
       * public surface can exercise it. It is kept because "the catch must not
       * throw" is the whole contract of a diagnostic, not because it is proven.
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
    Object.defineProperty(withReport, INSTALLED, { value: true });
    configure({ getElementError: withReport });
  }
}

export {};
