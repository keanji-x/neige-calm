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
 *    this file's own tests, unavoidably — but nothing anywhere asserts on their
 *    exact text; and
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
    const raw = element.getAttribute('class') ?? '';
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

  const cssHidden = (element: Element): boolean => {
    const style = element.ownerDocument.defaultView?.getComputedStyle(element);
    return style?.display === 'none' || style?.visibility === 'hidden';
  };

  /*
   * The CSS-hidden roots that hold something queryable.
   *
   * This exists because the attribute selector above is blind to exactly the
   * case #1161 needs: `ui/dialog/public.tsx:139` puts `display: none` on
   * `.dialog-body` whenever a child view is showing, and the `Create wave`
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
    const memo = new Map<Element, boolean>();
    const isHidden = (element: Element): boolean => {
      const seen = memo.get(element);
      if (seen !== undefined) return seen;
      const value = cssHidden(element);
      memo.set(element, value);
      return value;
    };
    const roots = new Map<Element, number>();
    for (const element of examined) {
      let root: Element | null = null;
      // `body` is included deliberately: a page whose own `body` is hidden read
      // as fully visible while the walk stopped one level short of it, which is
      // this file's own misdiagnosis class one level up.
      for (let node: Element | null = element; node !== null; node = node.parentElement) {
        if (isHidden(node)) root = node;
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
    const { body } = container.ownerDocument;
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
    const byAttribute = Array.from(body.querySelectorAll(HIDING_ATTRIBUTES))
      .filter((element) => element.querySelector(MEANINGFUL) !== null);
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
    const REPORT_PREFIX = `\n\n${MARKER} document.body children (`;
    const withoutReport = (text: string): string => {
      const at = text.indexOf(REPORT_PREFIX);
      return at === -1 ? text : text.slice(0, at);
    };

    const withReport = (message: string | null, container: Container) => {
      const error = inherited(message === null ? message : withoutReport(message), container);
      /*
       * Testing Library re-wraps messages that already came from here, and the
       * worst offender is the path #1161 actually takes: on timeout `waitFor`
       * calls `getElementError(lastError.message, …)`, so the report would be
       * appended to a message that already ends with one. `getMultipleError`
       * does it once per match on top of the outer wrap. Re-appending is only
       * noise, but it is noise in the one artifact this exists to be read from,
       * and it eats the runner's head+tail truncation budget.
       */
      /*
       * A diagnostic that throws would replace a real failure with a useless
       * one, so both halves give up quietly. Note the *second* `append` is why
       * this is two functions and not a bare try/catch: if the message could
       * not be written the first time — a frozen or read-only error from some
       * other `getElementError` — writing the failure notice would throw for
       * exactly the same reason, and the original error would be lost to a
       * `TypeError`. When nothing can be written, the error is returned
       * untouched, which is the outcome that loses the least.
       */
      try {
        append(error, report(container));
      } catch (cause) {
        append(error, `${MARKER} unavailable: ${String(cause)}`);
      }
      return error;
    };
    Object.defineProperty(withReport, INSTALLED, { value: true });
    configure({ getElementError: withReport });
  }
}

export {};
