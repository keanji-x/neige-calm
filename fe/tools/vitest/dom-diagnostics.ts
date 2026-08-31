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
 *  - the whole suite is green with it installed (1723 web-dom/node, 285
 *    browser), so nothing today reads a query-failure message; and
 *  - `getElementError` is called on *every* `waitFor` poll, not just the last
 *    one, so the cost matters — it is 0.045ms against the 4.04ms `prettyDOM`
 *    already spends per call on a 200-node body, i.e. about 1%.
 *
 * What it *would* break is a test that snapshots the exact text of a query
 * failure. None exists; one written later fails loudly rather than silently.
 */

// Only the DOM-bearing projects get this; `platform-independent` runs in node,
// where importing React Testing Library is both pointless and load-bearing on
// globals it does not have. The import is dynamic so the module is not even
// resolved there.
if (typeof document !== 'undefined') {
  const { configure, getConfig } = await import('@testing-library/react');

  /** Interactive-ish descendants; an element with none of these is decoration. */
  const MEANINGFUL = 'button, a[href], input, select, textarea, [role], [tabindex]';
  const BODY_CHILD_LIMIT = 8;
  const HIDDEN_SUBTREE_LIMIT = 8;
  const CLASS_CHARS = 60;

  const identify = (element: Element): string => {
    const tag = element.tagName.toLowerCase();
    const raw = element.getAttribute('class') ?? '';
    if (raw === '') return `<${tag}>`;
    const shown = raw.length > CLASS_CHARS ? `${raw.slice(0, CLASS_CHARS)}…(+${raw.length - CLASS_CHARS} chars)` : raw;
    return `<${tag} class="${shown}">`;
  };

  /*
   * `inert` and `aria-hidden` are the two ways a node stays in the DOM and
   * leaves the accessibility tree, and they are what every dialog/drawer in
   * this app writes onto its siblings. `display: none` is the third; it is read
   * off the computed style rather than the attribute because a stylesheet can
   * set it.
   */
  const hiddenBecause = (element: Element): string => {
    const reasons: string[] = [];
    if (element.hasAttribute('inert')) reasons.push('inert');
    if (element.getAttribute('aria-hidden') === 'true') reasons.push('aria-hidden');
    const view = element.ownerDocument.defaultView;
    if (view && view.getComputedStyle(element).display === 'none') reasons.push('display:none');
    return reasons.join('+');
  };

  const capped = <T>(items: readonly T[], limit: number, render: (item: T) => string): string => {
    const shown = items.slice(0, limit).map(render);
    if (items.length > limit) shown.push(`… and ${items.length - limit} more, not shown`);
    return shown.map((line) => `\n  ${line}`).join('');
  };

  const report = (container: Container): string => {
    // Testing Library always hands a live element; a detached one has no
    // `ownerDocument.body` to inventory, and the caller below turns that throw
    // into a stated "unavailable" rather than losing the underlying failure.
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
    const hiddenSubtrees = Array.from(body.querySelectorAll('[inert], [aria-hidden="true"]'))
      .filter((element) => element.querySelector(MEANINGFUL) !== null);
    const hidden = hiddenSubtrees.length === 0
      ? '\n  none'
      : capped(hiddenSubtrees, HIDDEN_SUBTREE_LIMIT, (element) => `${identify(element)} — ${hiddenBecause(element)}`);

    return [
      `[nc-a11y] document.body children (${children.length}):${inventory}`,
      `[nc-a11y] subtrees out of the accessibility tree that hold queryable elements `
        + `(${hiddenSubtrees.length}):${hidden}`,
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
    const withReport = (message: string | null, container: Container) => {
      const error = inherited(message, container);
      // A diagnostic that throws would replace a real failure with a useless
      // one, so its own failure is reported and swallowed.
      try {
        error.message = `${error.message}\n\n${report(container)}`;
      } catch (cause) {
        error.message = `${error.message}\n\n[nc-a11y] unavailable: ${String(cause)}`;
      }
      return error;
    };
    Object.defineProperty(withReport, INSTALLED, { value: true });
    configure({ getElementError: withReport });
  }
}

export {};
