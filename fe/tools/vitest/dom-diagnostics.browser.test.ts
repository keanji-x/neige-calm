/* Driven through the real query path, so the setup file's `configure()` is what is under test. A
 * `.browser.test` because jsdom's answers about the accessibility tree are not the platform's. */
import { afterEach, describe, expect, it } from 'vitest';
import { screen, waitFor } from '@testing-library/react';

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

/** The message of the error a failing query throws; a query that succeeds is reported, not hidden behind a timeout. */
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

/** Just the appended report: the DOM dump above it contains every fixture, so `toContain` on the whole message proves nothing. */
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

    expect(message).toContain('Unable to find an accessible element with the role "button"');
    expect(message).toContain('<button>');
    // The report is after DTL's text, which is what makes it survive the mutation runner's head+tail truncation.
    expect(message.indexOf('[nc-a11y]')).toBeGreaterThan(message.indexOf('Unable to find'));
  });

  it('names aria-hidden as the reason a present element is unqueryable', () => {
    mount('<div class="wrapper-under-test" aria-hidden="true"><button>Save</button></div>');
    const message = failureMessage(() => screen.getByRole('button', { name: 'Save' }));

    expect(reportOf(message)).toContain('<div class="wrapper-under-test"> — aria-hidden');
  });

  /* Testing Library's `isInaccessible` does not read `inert`, so a query keeps finding an inert element;
   * the first assertion records that disagreement so a change in DTL says so. */
  it('reports inert subtrees even though Testing Library does not treat them as hidden', () => {
    mount('<div class="inert-wrapper" inert><button>Save</button></div>');
    expect(screen.getByRole('button', { name: 'Save' })).toBeTruthy();

    expect(reportOf(failureMessage(missing))).toContain('<div class="inert-wrapper"> — inert');
  });

  /* `display: none` is what the attribute selector is blind to, and why the CSS scan exists. */
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

  /* `visibility`, not `display`: `display` is not inherited, so an inner wrapper never computes hidden
   * and cannot tell outermost-wins from innermost-wins. */
  it('names the outermost hidden ancestor once, not every element under it', () => {
    mount('<div class="outer-hidden" style="visibility: hidden"><div class="inner"><button>A</button>'
      + '<button>B</button><button>C</button></div></div>');
    const report = reportOf(failureMessage(missing));

    expect(report).toContain('<div class="outer-hidden"> — visibility:hidden (holds 3 queryable)');
    expect(report).not.toContain('class="inner"');
  });

  /* `visibility` is inherited but overridable, so the element, not the ancestor, must be trusted. */
  it('does not call an element hidden when it overrides visibility back to visible', () => {
    // The genuinely hidden sibling is the positive control: without it a scan
    // that found *nothing at all* would satisfy the absence assertion below.
    mount('<div class="veiled" style="visibility: hidden">'
      + '<button style="visibility: visible">Save</button></div>'
      + '<div class="really-veiled" style="visibility: hidden"><button>Nope</button></div>');
    const report = reportOf(failureMessage(missing));

    expect(report).toContain('<div class="really-veiled"> — visibility:hidden');
    expect(report).not.toContain('class="veiled"');
  });

  /* `querySelector` does not match the element it is called on. */
  it('counts a hidden element that is itself the queryable one', () => {
    mount('<button class="hidden-button" aria-hidden="true">Save</button>');
    const report = reportOf(failureMessage(missing));

    expect(report).toContain('<button class="hidden-button"> — aria-hidden');
  });

  /* DTL builds "Found multiple elements" by calling `getElementError(null, element)` once per match;
   * appending a report to each put reports between the dumps and the strip then cut at the first one. */
  it('leaves the multiple-matches message whole', () => {
    mount('<p>dupdup</p><p>dupdup</p>');
    const message = failureMessage(() => screen.getByText('dupdup'));

    expect(message).toContain('If this is intentional');
    // Three dumps: one per match plus the container dump the outer wrap adds.
    expect(message.split('Ignored nodes:').length - 1).toBe(3);
    expect(message.split(`${MARKER} document.body children`).length - 1).toBe(1);
  });

  /* `wait-for.js` passes `error.message` of whatever the callback threw; a non-`Error` leaves it undefined,
   * and a throw inside `onTimeout`'s `setTimeout` hangs the promise instead of failing it. */
  it('lets waitFor settle when its callback throws a non-Error', async () => {
    const outcome = await Promise.race([
      // Throwing a non-Error is the entire subject of this test: it is what
      // leaves `error.message` undefined on Testing Library's timeout path.
      // eslint-disable-next-line @typescript-eslint/only-throw-error -- see above
      waitFor(() => { throw { code: 1 }; }, { timeout: 50, interval: 10 })
        .then(() => 'resolved', () => 'rejected'),
      new Promise((resolve) => { setTimeout(() => resolve('never settled'), 1_500); }),
    ]);

    expect(outcome).toBe('rejected');
  });

  /* `querySelectorAll` cannot return the element it is called on, so an `aria-hidden` body was invisible to the attribute scan. */
  it('names body itself when body carries a hiding attribute', () => {
    mount('<button>Save</button>');
    document.body.setAttribute('aria-hidden', 'true');
    try {
      expect(reportOf(failureMessage(missing))).toContain('<body> — aria-hidden');
    } finally {
      document.body.removeAttribute('aria-hidden');
    }
  });

  /* The scan is rooted at the document, so `<html>` needs no special case. */
  it('names the documentElement when the hiding attribute is on <html>', () => {
    mount('<button>Save</button>');
    document.documentElement.setAttribute('aria-hidden', 'true');
    try {
      expect(reportOf(failureMessage(missing))).toContain('<html> — aria-hidden');
    } finally {
      document.documentElement.removeAttribute('aria-hidden');
    }
  });

  /* A class value may contain a newline, but the report's lines must stay single-line: `REPORT_TAIL`
   * recognises a previous report by its indented shape. */
  it('collapses whitespace in class names so the report stays strippable', async () => {
    const host = mount('<div><button>Save</button></div>');
    host.firstElementChild?.setAttribute('aria-hidden', 'true');
    host.firstElementChild?.setAttribute('class', 'a\nb');
    let message = '';
    try {
      await screen.findByRole('button', { name: 'Save' }, { timeout: 60, interval: 20 });
    } catch (error) {
      message = (error as Error).message;
    }

    expect(message).toContain('<div class="a b"> — aria-hidden');
    expect(message.split(`${MARKER} document.body children`).length - 1).toBe(1);
  });

  /* A query's own search string is printed back in the message, so the strip is end-anchored. */
  it('keeps content that merely looks like a report in the middle of a message', () => {
    mount('<button>Save</button>');
    const decoy = `\n\n${MARKER} document.body children (5):\nTAIL-MUST-SURVIVE`;
    const message = failureMessage(() => screen.getByText(decoy));

    /* DTL prints the normalized text first and the raw one after, so `TAIL-MUST-SURVIVE` appears earlier
     * and proves nothing; DTL's own closing sentence really is last. */
    expect(message).toContain('This could be because the text is broken up by multiple elements');
  });

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

  /* Asserted on position, not only count: a report stranded mid-message is what head+tail truncation would discard. */
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

  /* The strip is prefix-exact, not a marker search. */
  it('does not truncate a message whose own query text contains the marker', () => {
    mount('<div aria-hidden="true"><button>Save</button></div>');
    const message = failureMessage(() => screen.getByText(`${MARKER} not present`));

    // Searching for the bare marker would cut DTL's own sentence off here while still appending a report.
    expect(message).toContain(`Unable to find an element with the text: ${MARKER} not present`);
    expect(reportOf(message)).toContain('document.body children');
  });

  /* The queryable sibling is the positive control: an always-empty list would also satisfy the absence assertion. */
  it('leaves decoration out while still listing a real hidden subtree', () => {
    mount('<div class="decorative-wrapper" aria-hidden="true"><svg></svg></div>'
      + '<div class="substantive-wrapper" aria-hidden="true"><button>Save</button></div>');
    const report = reportOf(failureMessage(missing));

    expect(report).toContain('<div class="substantive-wrapper"> — aria-hidden');
    expect(report).not.toContain('decorative-wrapper');
  });

  /* Headings have an implicit role and no `role` attribute. */
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
    // Both captures are asserted to exist first: `toBe` uses `Object.is`, under which `NaN` equals `NaN`,
    // so a report stating no count at all would pass the comparison.
    expect(count(before)).toMatch(/^\d+$/);
    expect(count(after)).toMatch(/^\d+$/);
    expect(Number(count(before))).toBe(Number(count(after)) + 1);
  });

  /* The query string defeats the module cache, so the setup module's top-level body runs again against
   * a config that already holds the wrapper. */
  it('does not stack a second report when the setup module is evaluated again', async () => {
    // @ts-expect-error -- a Vite cache-busting specifier, not a path TypeScript
    // can resolve; the directive also fails loudly if that ever changes.
    await import('./dom-diagnostics.ts?evaluated-again');
    mount('<div aria-hidden="true"><button>Save</button></div>');
    const message = failureMessage(() => screen.getByRole('button', { name: 'Save' }));

    expect(message.split('[nc-a11y] document.body children').length - 1).toBe(1);
  });

  /* The failure notice is the same assignment to the same frozen object, so a try/catch that reported
   * by appending would throw out of the catch; the original error unchanged loses the least. */
  it('returns the original error untouched when its message cannot be written', async () => {
    const { configure, getConfig } = await import('@testing-library/react');
    const installed = getConfig().getElementError;
    /* One producer serving both halves: the writable branch is the positive control that proves the wrapper's body runs. */
    configure({
      getElementError: (message: string | null) => (message?.includes('FROZEN') === true
        ? Object.freeze(new Error('frozen baseline'))
        : new Error(String(message))),
    });
    try {
      // A fresh evaluation wraps the producer that was just installed.
      // @ts-expect-error -- a Vite cache-busting specifier, not a resolvable path.
      await import('./dom-diagnostics.ts?frozen-probe');
      expect(Symbol.for('nc.a11y-diagnostics.installed') in getConfig().getElementError).toBe(true);
      // Positive control: the writable error does get a report, so the wrapper
      // is not merely installed but doing its work.
      expect(reportOf(failureMessage(() => screen.getByText('writable')))).toContain('document.body children');
      // And the frozen one comes back exactly as the producer made it.
      expect(failureMessage(() => screen.getByText('FROZEN'))).toBe('frozen baseline');
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

  /* Deferring every build reported an empty page for a synchronous `getBy*` torn down in `finally`
   * before Vitest read `.message`. The discriminator is `_disableExpensiveErrorDiagnostics`, which
   * only `wait-for.js` sets around a poll. */
  it('reports the DOM eagerly when a synchronous query fails', () => {
    const host = mount('<div class="unmounted-before-read" aria-hidden="true"><button>Save</button></div>');
    let caught: Error | undefined;
    try {
      screen.getByRole('button', { name: 'Save' });
    } catch (error) {
      caught = error as Error;
    } finally {
      // Stands in for `app.dispose()`: the failure's evidence is gone from the
      // document by the time anything reads the message.
      host.remove();
    }
    // `toBeDefined` first, so a query that stopped failing cannot reach the
    // assertions below as an optional-chained `undefined`.
    expect(caught).toBeDefined();
    const message = caught!.message;

    // Testing Library's own dump, and this file's report, both describing the
    // UI as it was at the throw rather than the torn-down body.
    expect(message).toContain('<button>');
    expect(reportOf(message)).toContain('<div class="unmounted-before-read"> — aria-hidden');
    expect(reportOf(message)).not.toContain('document.body children (0)');
  });

  it('still defers the report on a waitFor poll', async () => {
    const host = mount('<div class="polled-then-unmounted" aria-hidden="true"><button>Save</button></div>');
    /* The error from a poll, not the timeout re-wrap: `wait-for.js` drops every poll error but the last,
     * so it must cost nothing. `.message` is deliberately not read inside the callback. */
    let polled: Error | undefined;
    try {
      await waitFor(() => {
        try {
          screen.getByRole('button', { name: 'Save' });
        } catch (error) {
          polled ??= error as Error;
          throw error;
        }
      }, { timeout: 60, interval: 20 });
    } catch { /* the timeout is expected; this test is about `polled`. */ }
    expect(polled).toBeDefined();

    host.remove();
    /* Reading only now: a deferred build runs against the page as it is here, so the fixture is absent.
     * If this ever reads back the fixture, the poll path went eager again. */
    expect(polled!.message).not.toContain('<div class="polled-then-unmounted">');
  });
});
