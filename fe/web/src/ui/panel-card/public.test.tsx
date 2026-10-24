// @vitest-environment jsdom

import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';

import { ContextGroup } from './context.tsx';
import { PanelModule } from './public.tsx';

afterEach(cleanup);

describe('PanelModule module marker', () => {
  it('puts the value on the module section', () => {
    const { container } = render(<PanelModule title="Cards" moduleMarker="cards">rows</PanelModule>);
    const section = container.querySelector('section');
    expect(section?.getAttribute('data-nc-module')).toBe('cards');
    /* On the section itself: the marker is the module layer's scope boundary. */
    expect(container.querySelectorAll('[data-nc-module]').length).toBe(1);
  });
});

describe('PanelModule title field marker', () => {
  it('puts the value on the heading that carries the title', () => {
    const { container } = render(
      <PanelModule title="Cards" titleFieldMarker="module-title">rows</PanelModule>,
    );
    const heading = container.querySelector('h2');
    expect(heading?.getAttribute('data-nc-field')).toBe('module-title');
    expect(heading?.textContent).toBe('Cards');
    expect(container.querySelectorAll('[data-nc-field]').length).toBe(1);
  });
});

describe.each([true, 'controls'] as const)('ContextGroup disclosure (%s)', (collapsible) => {
  it('honors controlled state without unmounting the body or nesting action buttons', () => {
    const onOpenChange = vi.fn();
    const onMenu = vi.fn();
    const content = (open: boolean) => <ContextGroup title="Resources" collapsible={collapsible} open={open}
      onOpenChange={onOpenChange} action={<button type="button" onClick={onMenu}>Menu</button>}>
      <input aria-label="Retained draft" defaultValue="keep me" />
    </ContextGroup>;
    const view = render(content(true));
    const toggle = screen.getByRole('button', { name: 'Resources' });
    const draft = screen.getByRole('textbox', { name: 'Retained draft' });
    const body = document.getElementById(toggle.getAttribute('aria-controls')!)!;
    const menu = screen.getByRole('button', { name: 'Menu' });
    expect(toggle.contains(menu)).toBe(false);
    fireEvent.click(toggle);
    expect(onOpenChange).toHaveBeenLastCalledWith(false);
    expect(toggle.getAttribute('aria-expanded')).toBe('true');
    view.rerender(content(false));
    expect(body.hidden).toBe(true);
    expect(draft.isConnected).toBe(true);
    fireEvent.click(menu);
    expect(onMenu).toHaveBeenCalledOnce();
    expect(onOpenChange).toHaveBeenCalledTimes(1);
    fireEvent.click(toggle);
    expect(onOpenChange).toHaveBeenLastCalledWith(true);
    view.rerender(content(true));
    expect(body.hidden).toBe(false);
    expect(screen.getByRole('textbox', { name: 'Retained draft' })).toBe(draft);
  });

  it('honors controlled disclosure without header actions and retains the body', () => {
    const onOpenChange = vi.fn();
    const content = (open: boolean) => <ContextGroup title="Plain" collapsible={collapsible} open={open} onOpenChange={onOpenChange}>
      <input aria-label="Draft" defaultValue="retained" />
    </ContextGroup>;
    const view = render(content(false));
    const toggle = screen.getByRole('button', { name: 'Plain' });
    const body = document.getElementById(toggle.getAttribute('aria-controls')!)!;
    const draft = body.querySelector('input');
    expect(body.hidden).toBe(true);
    fireEvent.click(toggle);
    expect(onOpenChange).toHaveBeenCalledExactlyOnceWith(true);
    expect(body.hidden).toBe(true);
    view.rerender(content(true));
    expect(body.hidden).toBe(false);
    expect(screen.getByRole('textbox', { name: 'Draft' })).toBe(draft);
    fireEvent.click(toggle);
    expect(onOpenChange).toHaveBeenLastCalledWith(false);
    view.rerender(content(false));
    expect(body.hidden).toBe(true);
    expect(draft?.isConnected).toBe(true);
  });

  it('keeps heading and uncontrolled disclosure available without header actions', () => {
    render(<ContextGroup title="Resources" collapsible={collapsible} open>Body</ContextGroup>);
    expect(screen.getByRole('region', { name: 'Resources' })).toBeTruthy();
    expect(screen.getByRole('heading', { name: 'Resources' })).toBeTruthy();
    const toggle = screen.getByRole('button', { name: 'Resources' });
    fireEvent.click(toggle);
    expect(toggle.getAttribute('aria-expanded')).toBe('false');
    expect(document.getElementById(toggle.getAttribute('aria-controls')!)?.hidden).toBe(true);
  });
});
