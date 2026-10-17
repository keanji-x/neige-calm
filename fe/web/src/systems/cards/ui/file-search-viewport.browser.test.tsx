import '../../../styles/entry.css';
import { act, cleanup, render, screen, waitFor } from '@testing-library/react';
import { EditorView } from '@codemirror/view';
import { afterEach, expect, it } from 'vitest';
import { page, userEvent } from 'vitest/browser';
import { BoardHost, createCardHost, createCardRegistry, registerAvailableBuiltinCards, type BoardHostItem } from '../public.ts';

afterEach(() => { cleanup(); document.body.replaceChildren(); });

async function paint() {
  await act(async () => { await new Promise<void>(resolve => requestAnimationFrame(() => requestAnimationFrame(() => resolve()))); });
}

function assertUsable(view: EditorView, board: HTMLElement) {
  const clip = board.getBoundingClientRect();
  const editor = view.scrollDOM.getBoundingClientRect();
  for (const control of [screen.getByRole('searchbox', { name: 'Search in file' }),
    screen.getByRole('button', { name: 'Next match' }), screen.getByRole('button', { name: 'Previous match' }),
    screen.getByRole('button', { name: 'Close search' })]) {
    const box = control.getBoundingClientRect();
    expect(box.top).toBeGreaterThanOrEqual(clip.top);
    expect(box.left).toBeGreaterThanOrEqual(clip.left);
    expect(box.right).toBeLessThanOrEqual(clip.right);
    expect(box.bottom).toBeLessThanOrEqual(clip.bottom);
    expect(control.contains(document.elementFromPoint((box.left + box.right) / 2, (box.top + box.bottom) / 2))).toBe(true);
  }
  const selected = view.state.selection.main;
  const start = view.domAtPos(selected.from), end = view.domAtPos(selected.to);
  const range = document.createRange();
  range.setStart(start.node, start.offset); range.setEnd(end.node, end.offset);
  expect(range.toString()).toBe('needle');
  const fragments = [...range.getClientRects()].filter(box => box.width > 0 && box.height > 0);
  expect(fragments.length).toBeGreaterThan(0);
  for (const box of fragments) {
    expect(box.top).toBeGreaterThanOrEqual(Math.max(clip.top, editor.top));
    expect(box.left).toBeGreaterThanOrEqual(Math.max(clip.left, editor.left));
    expect(box.right).toBeLessThanOrEqual(Math.min(clip.right, editor.right));
    expect(box.bottom).toBeLessThanOrEqual(Math.min(clip.bottom, editor.bottom));
    expect(view.contentDOM.contains(document.elementFromPoint((box.left + box.right) / 2, (box.top + box.bottom) / 2))).toBe(true);
  }
}

it.each([false, true])('keeps keyboard search reachable in an oversized real file card (user resized: %s)', async resized => {
  await page.viewport(1400, 900);
  const registry = createCardRegistry(); registerAvailableBuiltinCards(registry);
  const text = Array.from({ length: 8000 }, (_, index) => `row ${index}: needle`).join('\n');
  const host = createCardHost(registry, { files: {
    listDirectory: path => Promise.resolve({ path, parent: '/', entries: [{ name: 'large.txt', is_dir: false }] }),
    readFile: path => Promise.resolve({ path, size: text.length, text, truncated: false }),
    gitStatus: () => Promise.resolve({ repo_root: '/repo', files: [] }),
    gitDiff: () => Promise.reject(new Error('No diff expected')), rawUrl: path => path,
  } });
  const card = registry.resolve({ id: 'file', kind: 'file-viewer', payload: { path: '/repo' } });
  if (card === null) throw new Error('Real file card missing');
  const items: readonly BoardHostItem[] = [{ card, title: 'File', originalIndex: 0, activity: null, notice: null }];
  const scene = (visible: boolean, active = 'file', cards = items) => <div style={{ display: visible ? 'flex' : 'none', width: 1200, height: 'calc(100vh - 132px)' }}>
    <BoardHost host={host} items={cards} activeCardId={active} visible={visible} />
  </div>;
  const rendered = render(scene(true));
  await page.getByRole('button', { name: /large\.txt/ }).click();
  const content = await screen.findByRole('textbox', { name: 'File code' });
  const view = EditorView.findFromDOM(content);
  if (view === null) throw new Error('Real editor missing');
  const cell = document.querySelector<HTMLElement>('[data-nc-card-id="file"]')!;
  if (!resized) {
    expect(document.querySelector('[data-nc-file-card]')!.getBoundingClientRect().height).toBe(cell.getBoundingClientRect().height);
  }
  if (resized) {
    const handle = cell.querySelector<HTMLElement>('[data-nc-card-resize="se"]')!;
    const box = handle.getBoundingClientRect();
    const point = { clientX: box.left + box.width / 2, clientY: box.top + box.height / 2 };
    act(() => { handle.dispatchEvent(new MouseEvent('mousedown', { bubbles: true, button: 0, buttons: 1, ...point })); });
    act(() => { document.dispatchEvent(new MouseEvent('mousemove', { bubbles: true, buttons: 1, clientX: point.clientX, clientY: point.clientY + 270 })); });
    act(() => { document.dispatchEvent(new MouseEvent('mouseup', { bubbles: true, button: 0, clientX: point.clientX, clientY: point.clientY + 270 })); });
    await paint();
  }
  const cardHeight = cell.getBoundingClientRect().height;
  expect(cardHeight).toBeGreaterThan(resized ? 800 : 600);
  await page.viewport(1400, 500);
  await paint();
  const board = document.querySelector<HTMLElement>('[data-nc-card-board]')!;
  expect(board.clientHeight).toBe(368);
  content.focus();
  await userEvent.keyboard('{Control>}{Home}{/Control}/');
  const search = screen.getByRole('searchbox', { name: 'Search in file' });
  await page.getByRole('searchbox', { name: 'Search in file' }).fill('needle');
  await waitFor(() => { expect(screen.getByText('1/8000')).toBeDefined(); });
  await paint(); assertUsable(view, board);
  for (const [keys, count] of [['{Enter}', '2/8000'], ['{Shift>}{Enter}{/Shift}', '1/8000'],
    ['{Shift>}{Enter}{/Shift}', '8000/8000'], ['{Enter}', '1/8000']] as const) {
    await userEvent.keyboard(keys);
    await waitFor(() => { expect(screen.getByText(count)).toBeDefined(); });
    await paint(); assertUsable(view, board);
    expect(document.activeElement).toBe(search);
  }
  expect(cell.getBoundingClientRect().height).toBe(cardHeight);
  expect(view.state.doc.toString()).toBe(text);
  // A user can still scroll the board; resizing and visibility do not discard the grid dimensions.
  board.scrollTop = 100;
  await paint();
  expect(board.scrollTop).toBeGreaterThan(0);
  await userEvent.keyboard('{Shift>}{Enter}{/Shift}');
  await paint(); assertUsable(view, board);
  const sibling = registry.resolve({ id: 'terminal', kind: 'terminal', payload: {} });
  if (sibling === null) throw new Error('Real terminal card missing');
  const withSibling: readonly BoardHostItem[] = [...items, { card: sibling, title: 'Terminal', originalIndex: 1, activity: null, notice: null }];
  rendered.rerender(scene(true, 'terminal', withSibling));
  await paint();
  const terminal = document.querySelector<HTMLElement>('[data-nc-card-id="terminal"]')!;
  const terminalSurface = terminal.querySelector('[data-nc-terminal-card]')!;
  expect(terminalSurface.getBoundingClientRect().height).toBe(terminal.getBoundingClientRect().height);
  const clip = board.getBoundingClientRect();
  expect(terminalSurface.getBoundingClientRect().top).toBeGreaterThanOrEqual(clip.top - 1);
  expect(terminalSurface.getBoundingClientRect().top).toBeLessThan(clip.bottom);
  rendered.rerender(scene(true, 'file', withSibling));
  await paint(); assertUsable(view, board);
  rendered.rerender(scene(false, 'file', withSibling));
  await page.viewport(1400, 600); await paint();
  rendered.rerender(scene(true, 'file', withSibling));
  await paint(); assertUsable(view, board);
  expect(cell.getBoundingClientRect().height).toBe(cardHeight);
  await page.viewport(1400, 900); await paint();
  expect(cell.getBoundingClientRect().height).toBe(cardHeight);
});
