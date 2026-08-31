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

  it('leaves decoration out: a hidden subtree with nothing queryable in it', () => {
    mount('<div class="decorative-wrapper" aria-hidden="true"><svg></svg></div>');
    const message = failureMessage(missing);

    expect(reportOf(message)).toContain('subtrees out of the accessibility tree');
    expect(reportOf(message)).not.toContain('decorative-wrapper');
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

  it('announces its own truncation instead of silently dropping subtrees', () => {
    // Nine hidden subtrees against a limit of eight: the smallest input that
    // can overflow, so the message cannot be produced by an off-by-one.
    mount(Array.from({ length: 9 }, (_, index) =>
      `<div class="overflow-${index}" aria-hidden="true"><button>b${index}</button></div>`).join(''));
    const report = reportOf(failureMessage(missing));

    expect(report).toContain('(9):');
    expect(report).toContain('… and 1 more, not shown');
  });
});
