import '../../styles/entry.css';
import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, expect, it } from 'vitest';
import { page } from 'vitest/browser';

import type { WorkspaceFilePort } from '../../../../core/domain/fs.ts';
import { ReportDocument } from '../../features/report/document/public.tsx';
import { createCardHost } from '../../systems/cards/host.ts';
import { createCardRegistry } from '../../systems/cards/registry.ts';
import { registerAvailableBuiltinCards } from '../../systems/cards/builtins/register.ts';
import { BoardHost } from '../../systems/cards/ui/board-host.tsx';
import { Dialog } from '../dialog/public.tsx';
import { Drawer } from '../drawer/public.tsx';
import { PanelCard } from '../panel-card/public.tsx';

let previousTheme: string | null;
beforeEach(async () => { previousTheme = document.documentElement.getAttribute('data-theme'); await page.viewport(1400, 900); });
afterEach(() => {
  cleanup();
  if (previousTheme === null) document.documentElement.removeAttribute('data-theme');
  else document.documentElement.setAttribute('data-theme', previousTheme);
});

function reference(theme: 'light' | 'dark') {
  document.documentElement.setAttribute('data-theme', theme);
  render(<div style={{ position: 'relative', width: 1200, height: 700, containerType: 'inline-size' }}>
    <Drawer open title="Shared card reference" onClose={() => {}}>Conversation</Drawer>
  </div>);
  const style = getComputedStyle(screen.getByRole('complementary', { name: 'Shared card reference' }));
  expect(style.borderRadius).toBe('16px');
  expect(style.boxShadow).not.toBe('none');
  return style;
}

function expectFrame(element: Element, expected: CSSStyleDeclaration, flat = false) {
  const actual = getComputedStyle(element);
  expect(actual.borderRadius).toBe(expected.borderRadius);
  expect(actual.boxShadow).toBe(flat ? 'none' : expected.boxShadow);
  expect(actual.borderWidth).toBe(expected.borderWidth);
  expect(actual.backgroundColor).toBe(expected.backgroundColor);
  expect(actual.overflow).toBe('hidden');
}

for (const theme of ['light', 'dark'] as const) {
  it(`shares the conversation frame with registered Terminal and File cards (${theme})`, async () => {
    const expected = reference(theme);
    const registry = createCardRegistry();
    registerAvailableBuiltinCards(registry);
    const cards = [
      registry.resolve({ id: 'terminal-frame', kind: 'terminal', payload: {} }),
      registry.resolve({ id: 'file-frame', kind: 'file-viewer', payload: { path: '/repo' } }),
    ];
    if (cards.some(card => card === null)) throw new Error('Missing builtin card');
    const items = cards.map((card, originalIndex) => {
      if (card === null) throw new Error('Missing builtin card');
      return { card, title: card.id, originalIndex, activity: null, notice: null };
    });
    render(<div style={{ width: 1200, height: 700 }}><BoardHost host={createCardHost(registry)} items={items} activeCardId={null} visible /></div>);
    await waitFor(() => expect(document.querySelector('[data-nc-file-card]')).not.toBeNull());
    expectFrame(document.querySelector('[data-nc-terminal-card]')!, expected);
    expectFrame(document.querySelector('[data-nc-file-card]')!, expected);
    const terminal = document.querySelector<HTMLElement>('[data-nc-terminal-card]')!;
    const cell = terminal.closest<HTMLElement>('[data-nc-card-cell]')!;
    const handle = terminal.querySelector<HTMLElement>('[data-nc-card-drag]')!;
    const box = handle.getBoundingClientRect();
    const point = { clientX: box.left + 20, clientY: box.top + 10 };
    try {
      act(() => { handle.dispatchEvent(new MouseEvent('mousedown', { bubbles: true, button: 0, buttons: 1, ...point })); });
      act(() => { document.dispatchEvent(new MouseEvent('mousemove', { bubbles: true, buttons: 1, clientX: point.clientX, clientY: point.clientY + 60 })); });
      expect(cell.classList.contains('react-draggable-dragging')).toBe(true);
      expect(getComputedStyle(cell).boxShadow).toBe('none');
      expectFrame(terminal, expected);
    } finally {
      act(() => { document.dispatchEvent(new MouseEvent('mouseup', { bubbles: true, button: 0, clientX: point.clientX, clientY: point.clientY + 60 })); });
    }
  });

  it(`shares the conversation frame with real file link previews (${theme})`, async () => {
    const expected = reference(theme);
    const files: WorkspaceFilePort = {
      readFile: path => Promise.resolve({ path, text: '# File contents', size: 15, truncated: false }),
      rawUrl: path => `/workspace/${path}`,
    };
    const report = { summary: '', body: '[Notes](./notes.md)', blocks: null };
    render(<ReportDocument report={report} empty={null} fileRoot="/repo"
      linkPreview={{ files, trackId: 'frame-test', report }} onOpenFileLink={() => {}} />);
    fireEvent.keyDown(screen.getByRole('button', { name: 'Notes' }), { key: 'ArrowDown' });
    const preview = await screen.findByRole('dialog', { name: 'Preview: Notes' });
    await screen.findByRole('heading', { name: 'File contents' });
    expectFrame(preview, expected);
  });

  it(`shares the conversation frame with modal card surfaces (${theme})`, () => {
    const expected = reference(theme);
    render(<Dialog origin={{ kind: 'surface-center' }} open title="Files" onClose={() => {}}><button type="button">Choose file</button></Dialog>);
    expectFrame(screen.getByRole('dialog', { name: 'Files' }), expected);
  });

  it(`uses the same frame without elevation for context panels (${theme})`, () => {
    const expected = reference(theme);
    render(<section aria-label="Context panel"><PanelCard>Context</PanelCard></section>);
    expectFrame(screen.getByRole('region', { name: 'Context panel' }).firstElementChild!, expected, true);
  });
}
