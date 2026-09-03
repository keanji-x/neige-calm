// @vitest-environment jsdom
//
// #1234 S1b-3b — the three projection marker channels this primitive opened.
//
// Each channel gets two assertions and needs both: that the marker lands on the
// **right element** (a `data-nc-module` on the head row instead of the section
// would satisfy a "the attribute is somewhere" check and break the projection's
// scoping), and that **omitting the prop leaves no attribute at all**. The
// second one is not symmetry for its own sake: the track page renders
// `Referenced by` and `Conversations` through `PanelModule` as well, and those
// two must stay unmarked or the panel's tree holds four module markers against a
// view model of two.

import { cleanup, render } from '@testing-library/react';
import { afterEach, describe, expect, it } from 'vitest';

import { PanelEmpty, PanelModule } from './public.tsx';

afterEach(cleanup);

describe('PanelModule module marker', () => {
  it('puts the value on the module section', () => {
    const { container } = render(<PanelModule title="Cards" moduleMarker="cards">rows</PanelModule>);
    const section = container.querySelector('section');
    expect(section?.getAttribute('data-nc-module')).toBe('cards');
    /* On the section itself, not on some descendant that happens to be inside
       it — the marker is the module layer's scope boundary. */
    expect(container.querySelectorAll('[data-nc-module]').length).toBe(1);
  });

  it('emits no attribute when the prop is omitted', () => {
    const { container } = render(<PanelModule title="Conversations">rows</PanelModule>);
    expect(container.querySelector('[data-nc-module]')).toBeNull();
    expect(container.querySelector('section')?.hasAttribute('data-nc-module')).toBe(false);
  });
});

describe('PanelModule title field marker', () => {
  it('puts the value on the heading that carries the title', () => {
    const { container } = render(
      <PanelModule title="Cards" titleFieldMarker="module-title">rows</PanelModule>,
    );
    const heading = container.querySelector('h2');
    expect(heading?.getAttribute('data-nc-field')).toBe('module-title');
    /* The carrier owes an exact string, so it must be the element whose whole
       text is the title. */
    expect(heading?.textContent).toBe('Cards');
    expect(container.querySelectorAll('[data-nc-field]').length).toBe(1);
  });

  it('emits no attribute when the prop is omitted', () => {
    const { container } = render(<PanelModule title="Cards">rows</PanelModule>);
    expect(container.querySelector('[data-nc-field]')).toBeNull();
    expect(container.querySelector('h2')?.hasAttribute('data-nc-field')).toBe(false);
  });

  it('the two channels are independent', () => {
    const { container } = render(<PanelModule title="Cards" moduleMarker="cards">rows</PanelModule>);
    expect(container.querySelector('section')?.getAttribute('data-nc-module')).toBe('cards');
    expect(container.querySelector('h2')?.hasAttribute('data-nc-field')).toBe(false);
  });
});

describe('PanelEmpty field marker', () => {
  it('puts the value on the element that carries the sentence', () => {
    const { container } = render(<PanelEmpty fieldMarker="empty">No cards yet.</PanelEmpty>);
    const paragraph = container.querySelector('p');
    expect(paragraph?.getAttribute('data-nc-field')).toBe('empty');
    expect(paragraph?.textContent).toBe('No cards yet.');
    expect(container.querySelectorAll('[data-nc-field]').length).toBe(1);
  });

  it('emits no attribute when the prop is omitted', () => {
    const { container } = render(<PanelEmpty>No cards yet.</PanelEmpty>);
    expect(container.querySelector('[data-nc-field]')).toBeNull();
    expect(container.querySelector('p')?.hasAttribute('data-nc-field')).toBe(false);
  });
});
