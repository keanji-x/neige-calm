/*
 * The #1161 failure report, driven through the real query path.
 *
 * Every case here calls an actual `@testing-library` query and reads the
 * message off the error it throws, because the whole point of the module under
 * test is that it is installed on the *shipped* `getElementError` by a setup
 * file. A fixture that called `report()` directly would still pass if the
 * `configure()` call were deleted.
 *
 * This is a `.browser.test.ts` because `inert` and `aria-hidden` are
 * accessibility-tree semantics, and jsdom's answers about the accessibility
 * tree are not the platform's.
 */
import { afterEach, describe, expect, it } from 'vitest';
import { screen } from '@testing-library/react';

/** Marks every node this file adds, so cleanup cannot strand page chrome. */
const FIXTURE_CLASS = 'nc-a11y-fixture';
/** Must match `MARKER` in dom-diagnostics.ts. */
const MARKER = '[nc-a11y]';

const mount = (html: string): HTMLElement => {
  const host = document.createElement('div');
  host.className = FIXTURE_CLASS;
  host.innerHTML = html;
  document.body.append(host);
  return host;
};

/**
 * The message of the error a failing query throws. A query that *succeeds* is
 * itself a result worth asserting on, so this reports that rather than hiding
 * it behind a timeout.
 */
const failureMessage = (query: () => unknown): string => {
  try {
    query();
  } catch (error) {
    return (error as Error).message;
  }
  throw new Error('the query succeeded; there is no failure message to read');
};

/** A role nothing in the page or the fixtures ever has. */
const missing = () => screen.getByRole('meter', { name: 'nothing has this' });

/**
 * Just the appended report. Assertions have to be scoped to it: the DOM dump
 * above it contains every fixture this file mounts, so `toContain` against the
 * whole message would pass on the dump alone and prove nothing about the
 * report — which is how the decoration case first "passed".
 */
const reportOf = (message: string): string => {
  const start = message.indexOf('[nc-a11y]');
  expect(start).toBeGreaterThan(-1);
  return message.slice(start);
};

afterEach(() => {
  for (const node of Array.from(document.querySelectorAll(`.${FIXTURE_CLASS}`))) node.remove();
});

describe('the a11y failure report (#1161)', () => {
  it('is appended to the real Testing Library error rather than replacing it', () => {
    mount('<div aria-hidden="true"><button>Save</button></div>');
    const message = failureMessage(() => screen.getByRole('button', { name: 'Save' }));

    // Delegation: DTL's own text and its DOM dump are still there …
    expect(message).toContain('Unable to find an accessible element with the role "button"');
    expect(message).toContain('<button>');
    // … and the report is *after* them, which is what makes it survive the
    // mutation runner's head+tail truncation of captured failure messages.
    expect(message.indexOf('[nc-a11y]')).toBeGreaterThan(message.indexOf('Unable to find'));
  });

  it('names aria-hidden as the reason a present element is unqueryable', () => {
    mount('<div class="wrapper-under-test" aria-hidden="true"><button>Save</button></div>');
    const message = failureMessage(() => screen.getByRole('button', { name: 'Save' }));

    expect(reportOf(message)).toContain('<div class="wrapper-under-test"> — aria-hidden');
  });

  /*
   * The trap this exists for. `inert` removes a subtree from the accessibility
   * tree in a browser, but Testing Library's own `isInaccessible` does not read
   * it — it looks at `display`, `visibility`, `hidden` and `aria-hidden` only.
   * So a query keeps *finding* an inert element, and every dialog in this app
   * writes `inert` and `aria-hidden` together. The report states `inert`
   * because a human reading a CI failure needs to know it is there; the
   * assertion below records that DTL disagrees, so that if DTL ever starts
   * honouring `inert` this test says so instead of silently changing meaning.
   */
  it('reports inert subtrees even though Testing Library does not treat them as hidden', () => {
    mount('<div class="inert-wrapper" inert><button>Save</button></div>');
    expect(screen.getByRole('button', { name: 'Save' })).toBeTruthy();

    expect(reportOf(failureMessage(missing))).toContain('<div class="inert-wrapper"> — inert');
  });

  /*
   * The case the attribute selector is blind to, and the reason the CSS scan
   * exists. `ui/dialog/public.tsx:139` puts `display: none` on `.dialog-body`
   * whenever a child view is showing, and #1161's missing `Create wave` button
   * lives inside it — so a report that said "no hidden subtree holds anything
   * queryable" here would be actively wrong about the one thing it is for.
   */
  it('finds a subtree hidden by CSS, which no attribute selector can express', () => {
    mount('<div class="css-hidden-wrapper" style="display: none"><button>Save</button></div>');
    const report = reportOf(failureMessage(missing));

    expect(report).toContain('<div class="css-hidden-wrapper"> — display:none (holds 1 queryable)');
  });

  it('finds visibility:hidden and the hidden attribute too', () => {
    mount('<div class="invisible-wrapper" style="visibility: hidden"><button>A</button></div>'
      + '<div class="hidden-attr-wrapper" hidden><button>B</button></div>');
    const report = reportOf(failureMessage(missing));

    expect(report).toContain('<div class="invisible-wrapper"> — visibility:hidden');
    expect(report).toContain('<div class="hidden-attr-wrapper"> — hidden');
  });

  /*
   * The walk must name the subtree root, not each hidden button: a reader wants
   * "this container went away", not fifty lines of its contents.
   *
   * **`visibility`, not `display`.** The obvious `display: none` fixture cannot
   * tell outermost-wins from innermost-wins: `display` is not inherited, so an
   * inner wrapper never computes as hidden and is never a candidate root — the
   * assertion holds under either policy. `visibility: hidden` *is* inherited,
   * so `.inner` computes hidden too and the two policies give different
   * answers. Verified by mutation: switching the walk to keep the first hidden
   * ancestor instead of the highest turns this red.
   */
  it('names the outermost hidden ancestor once, not every element under it', () => {
    mount('<div class="outer-hidden" style="visibility: hidden"><div class="inner"><button>A</button>'
      + '<button>B</button><button>C</button></div></div>');
    const report = reportOf(failureMessage(missing));

    expect(report).toContain('<div class="outer-hidden"> — visibility:hidden (holds 3 queryable)');
    expect(report).not.toContain('class="inner"');
  });

  /*
   * A page whose own `body` is hidden. The ancestor walk used to stop one level
   * short of `body`, so this reported "none" — the whole page invisible and the
   * report saying nothing is out of the accessibility tree.
   */
  it('names body itself when the whole page is hidden', () => {
    mount('<button>Save</button>');
    const previous = document.body.style.display;
    document.body.style.display = 'none';
    try {
      expect(reportOf(failureMessage(missing))).toContain('<body> — display:none');
    } finally {
      document.body.style.display = previous;
    }
  });

  /*
   * The re-wrap path, asserted on *position* and not only on count. Declining to
   * re-append leaves the report stranded in the middle followed by a second
   * `prettyDOM` dump, which is precisely what the runner's head+tail truncation
   * would discard.
   */
  it('keeps the report last through the findBy timeout re-wrap', async () => {
    mount('<div aria-hidden="true"><button>Save</button></div>');
    let message = '';
    try {
      await screen.findByRole('button', { name: 'Save' }, { timeout: 60, interval: 20 });
    } catch (error) {
      message = (error as Error).message;
    }

    expect(message.split(`${MARKER} document.body children`).length - 1).toBe(1);
    // Nothing from Testing Library may follow the report.
    expect(message.lastIndexOf(MARKER)).toBeGreaterThan(message.lastIndexOf('Ignored nodes:'));
  });

  /*
   * The strip is prefix-exact rather than a marker search, so a query whose own
   * text contains the marker is not mistaken for a re-wrap and silently denied
   * its report.
   */
  it('does not truncate a message whose own query text contains the marker', () => {
    mount('<div aria-hidden="true"><button>Save</button></div>');
    const message = failureMessage(() => screen.getByText(`${MARKER} not present`));

    // The load-bearing half: searching for the bare marker instead of the exact
    // report prefix would cut Testing Library's own sentence off right here, and
    // the report would still be appended — so asserting only that a report
    // exists proves nothing.
    expect(message).toContain(`Unable to find an element with the text: ${MARKER} not present`);
    expect(reportOf(message)).toContain('document.body children');
  });

  /*
   * Both halves in one fixture on purpose. Asserting only that decoration is
   * absent passes just as well against a list that is *always* empty — which is
   * exactly what a gutted implementation produces. The queryable sibling is the
   * positive control that makes the absence mean something.
   */
  it('leaves decoration out while still listing a real hidden subtree', () => {
    mount('<div class="decorative-wrapper" aria-hidden="true"><svg></svg></div>'
      + '<div class="substantive-wrapper" aria-hidden="true"><button>Save</button></div>');
    const report = reportOf(failureMessage(missing));

    expect(report).toContain('<div class="substantive-wrapper"> — aria-hidden');
    expect(report).not.toContain('decorative-wrapper');
  });

  /*
   * Headings have an implicit role and no `role` attribute, so an earlier
   * version of `MEANINGFUL` called this subtree decoration and omitted it.
   */
  it('counts an implicit role as queryable, not as decoration', () => {
    mount('<div class="heading-wrapper" aria-hidden="true"><h1>Title</h1></div>');
    const report = reportOf(failureMessage(missing));

    expect(report).toContain('<div class="heading-wrapper"> — aria-hidden');
  });

  it('lists body children so an absent portal is distinguishable from a hidden one', () => {
    const host = mount('<span>anything</span>');
    const before = reportOf(failureMessage(missing));
    expect(before).toContain(`<div class="${FIXTURE_CLASS}">`);

    host.remove();
    const after = reportOf(failureMessage(missing));
    expect(after).not.toContain(`<div class="${FIXTURE_CLASS}">`);
    // The counts are the load-bearing half: "one body child" and "two body
    // children, the second one aria-hidden" are different diagnoses.
    const count = (message: string) => /document\.body children \((\d+)\)/.exec(message)?.[1];
    expect(Number(count(before))).toBe(Number(count(after)) + 1);
  });

  /*
   * The query string defeats the module cache, so the setup module's top-level
   * body runs a second time against a config that already holds the wrapper —
   * which is what a second `configure()` would do for real if `web-dom` ever
   * turns `isolate` off (#1123). Without the install guard the report is
   * appended twice.
   */
  it('does not stack a second report when the setup module is evaluated again', async () => {
    // @ts-expect-error -- a Vite cache-busting specifier, not a path TypeScript
    // can resolve; the directive also fails loudly if that ever changes.
    await import('./dom-diagnostics.ts?evaluated-again');
    mount('<div aria-hidden="true"><button>Save</button></div>');
    const message = failureMessage(() => screen.getByRole('button', { name: 'Save' }));

    expect(message.split('[nc-a11y] document.body children').length - 1).toBe(1);
  });

  /*
   * If the message cannot be written, the *failure notice* cannot be written
   * either — it is the same assignment to the same frozen object. A bare
   * try/catch that reports the problem by appending would therefore throw out
   * of the catch and replace a real query failure with a `TypeError`. The
   * original error, unchanged, is the outcome that loses the least.
   */
  it('returns the original error untouched when its message cannot be written', async () => {
    const { configure, getConfig } = await import('@testing-library/react');
    const installed = getConfig().getElementError;
    configure({ getElementError: () => Object.freeze(new Error('frozen baseline')) });
    try {
      // A fresh evaluation wraps the frozen-error producer that was just installed.
      // @ts-expect-error -- a Vite cache-busting specifier, not a resolvable path.
      await import('./dom-diagnostics.ts?frozen-probe');
      // Positive control: without this the test passes even if the fresh
      // evaluation installed nothing at all, because the frozen producer
      // already returns exactly this message.
      expect(Symbol.for('nc.a11y-diagnostics.installed') in getConfig().getElementError).toBe(true);
      expect(failureMessage(missing)).toBe('frozen baseline');
    } finally {
      configure({ getElementError: installed });
    }
  });

  it('announces its own truncation instead of silently dropping subtrees', () => {
    // Nine hidden subtrees against a limit of eight: the smallest input that
    // can overflow, so the message cannot be produced by an off-by-one.
    mount(Array.from({ length: 9 }, (_, index) =>
      `<div class="overflow-${index}" aria-hidden="true"><button>b${index}</button></div>`).join(''));
    const report = reportOf(failureMessage(missing));

    expect(report).toContain('(9, by inert/');
    expect(report).toContain('… and 1 more, not shown');
  });
});
