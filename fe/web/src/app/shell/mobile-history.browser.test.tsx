import '../../styles/entry.css';
import { cleanup, render } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import { page, userEvent } from 'vitest/browser';
import { MobileConversationProvider, useMobileConversationOwner, useMobileConversationScope } from './public.tsx';
import type { Conversation } from '../../../../core/domain/conversation.ts';
import { MobileHistory } from './mobile-history.tsx';

afterEach(() => cleanup());

function setup() {
  const onNew = vi.fn();
  const onOpenChange = vi.fn();
  const onOpenSettings = vi.fn();
  const onSelect = vi.fn();
  let open = true;
  let currentTrackId = 'A';
  let owner = {};
  const props = () => ({ owner, open, currentTrackId, onNew, onOpenChange,
    conversations: Array.from({ length: 30 }, (_, i) => ({ id: `chat-${i}`, trackId: currentTrackId, title: `Conversation ${i}`, kind: 'track-assistant' as const, provider: 'codex' as const, state: 'idle' as const, updatedAt: 0 })), loading: false, failed: false, onRetry: vi.fn(), onSelect,
    onOpenSettings, accountLabel: 'Account', accountInitial: 'A', onSignOut: vi.fn(), now: 0,
    scopeLabel: 'Current track', selectedConversationId: 'chat-0', trackGroups: { pinned: [], unread: [], waiting: [] },
    onOpenTrack: vi.fn(), isUnread: () => false, tracksLoading: false, tracksError: null, onRetryTracks: vi.fn(),
  });
  const view = render(<MobileHistory {...props()} />);
  return { onNew, onOpenSettings, onSelect, onOpenChange, view,
    setOpen(value: boolean) { open = value; view.rerender(<MobileHistory {...props()} />); },
    setOwner(value: string) { currentTrackId = value; owner = {}; view.rerender(<MobileHistory {...props()} />); },
    dialog: () => view.container.querySelector('dialog')!,
    admit() { view.getByRole('button', { name: '发起新对话' }).click(); },
  };
}

it('lifecycle-only: cancels New on reopen before a later ordinary native close', async () => {
  await page.viewport(390, 844);
  const history = setup();
  await expect.poll(() => history.dialog().open).toBe(true);
  history.admit();
  history.setOpen(false);
  history.setOpen(true);
  history.setOpen(false);
  // Public native close is a deterministic lifecycle boundary, not a user input test.
  const closed = new Promise<void>(resolve => history.dialog().addEventListener('close', () => resolve(), { once: true }));
  history.dialog().close();
  await closed;
  expect(history.onNew).not.toHaveBeenCalled();
});

it('lifecycle-only: cancels an old owner action before native close', async () => {
  await page.viewport(390, 844);
  const history = setup();
  await expect.poll(() => history.dialog().open).toBe(true);
  history.admit();
  history.setOpen(false);
  const old = history.dialog();
  const closed = nativeClose(old);
  history.setOwner('B');
  await closed;
  expect(history.onNew).not.toHaveBeenCalled();
});


function nativeClose(node: HTMLDialogElement) {
  const closed = new Promise<void>(resolve => node.addEventListener('close', () => resolve(), { once: true }));
  node.close();
  return closed;
}

it('new contract, lifecycle-only: first intent wins and revokes app interactions immediately', async () => {
  await page.viewport(390, 844);
  const history = setup();
  await expect.poll(() => history.dialog().open).toBe(true);
  const settings = history.view.getByRole('button', { name: '设置' });
  const view = history.view.getByRole('button', { name: '已置顶' });
  history.admit();
  expect(settings.closest('[inert]')).not.toBeNull();
  settings.click(); // Even a stale programmatic dispatch cannot replace New.
  view.click();
  expect(history.view.getByRole('button', { name: '本 Track' }).getAttribute('aria-pressed')).toBe('true');
  expect(history.onOpenChange).toHaveBeenCalledTimes(1);
  expect(history.onNew).not.toHaveBeenCalled();
  history.setOpen(false);
  await nativeClose(history.dialog());
  expect(history.onNew).toHaveBeenCalledTimes(1);
  expect(history.onOpenSettings).not.toHaveBeenCalled();
});

it('lifecycle-only: consumes before duplicate and reentrant native close delivery', async () => {
  await page.viewport(390, 844);
  const history = setup();
  await expect.poll(() => history.dialog().open).toBe(true);
  const node = history.dialog();
  history.onNew.mockImplementation(() => {
    expect(node.open).toBe(false);
    // The real action is invoked only after the native modal releases authority.
    if (history.onNew.mock.calls.length === 1) node.dispatchEvent(new Event('close'));
  });
  history.admit();
  history.setOpen(false);
  await nativeClose(node);
  node.dispatchEvent(new Event('close'));
  expect(history.onNew).toHaveBeenCalledTimes(1);
});

it('lifecycle-only: queued old-node close cannot consume a reopened presentation intent', async () => {
  await page.viewport(390, 844);
  const history = setup();
  await expect.poll(() => history.dialog().open).toBe(true);
  const old = history.dialog();
  history.admit();
  history.setOpen(false);
  const queued = nativeClose(old);
  history.setOpen(true);
  const current = history.dialog();
  expect(current).not.toBe(old);
  expect(current.open).toBe(true);
  history.admit();
  history.setOpen(false);
  await queued;
  expect(history.onNew).not.toHaveBeenCalled();
  await nativeClose(current);
  expect(history.onNew).toHaveBeenCalledTimes(1);
});

it.each(['same path new owner', 'A-B-A', 'unmount'] as const)('lifecycle-only: synchronously cancels %s before queued close', async change => {
  await page.viewport(390, 844);
  const history = setup();
  await expect.poll(() => history.dialog().open).toBe(true);
  history.admit();
  history.setOpen(false);
  const old = history.dialog();
  const queued = nativeClose(old);
  if (change === 'unmount') history.view.unmount();
  else {
    history.setOwner(change === 'A-B-A' ? 'B' : 'A');
    if (change === 'A-B-A') history.setOwner('A');
  }
  await queued;
  expect(history.onNew).not.toHaveBeenCalled();
});


it.each(['Escape', 'backdrop'] as const)('natural input: ordinary %s preserves native initial/return focus without actions', async method => {
  await page.viewport(390, 844);
  const opener = document.createElement('button');
  opener.textContent = 'Outside opener'; document.body.append(opener); opener.focus();
  try {
    const history = setup();
    history.onOpenChange.mockImplementation((open: boolean) => history.setOpen(open));
    await expect.poll(() => history.dialog().contains(document.activeElement)).toBe(true);
    if (method === 'Escape') await userEvent.keyboard('{Escape}');
    else await page.elementLocator(history.dialog()).click({ position: { x: 380, y: 400 } });
    await expect.poll(() => history.dialog().open).toBe(false);
    expect(history.onNew).not.toHaveBeenCalled();
    expect(history.onOpenSettings).not.toHaveBeenCalled();
    expect(document.activeElement).toBe(opener);
  } finally { opener.remove(); }
});

it('natural input: retains current list scroll and selected row through native presentation replacement', async () => {
  await page.viewport(390, 844);
  const history = setup();
  history.onOpenChange.mockImplementation((open: boolean) => history.setOpen(open));
  await expect.poll(() => history.dialog().open).toBe(true);
  const old = history.dialog();
  const list = history.view.getByRole('region', { name: 'Active conversations' }).parentElement!;
  list.scrollTop = 120;
  expect(list.scrollTop).toBe(120);
  expect(history.view.getByRole('button', { name: 'Conversation 0' }).getAttribute('aria-current')).toBe('true');
  await userEvent.keyboard('{Escape}');
  await expect.poll(() => old.open).toBe(false);
  history.setOpen(true);
  expect(history.dialog()).not.toBe(old);
  expect(history.view.getByRole('region', { name: 'Active conversations' }).parentElement!.scrollTop).toBe(120);
  expect(history.view.getByRole('button', { name: 'Conversation 0' }).getAttribute('aria-current')).toBe('true');
  await page.getByRole('button', { name: '已置顶', exact: true }).click();
  await userEvent.keyboard('{Escape}');
  await expect.poll(() => history.dialog().open).toBe(false);
  history.setOpen(true);
  expect(history.view.getByRole('button', { name: '本 Track' }).getAttribute('aria-pressed')).toBe('true');
});

it('natural input: New runs after native release and later close never steals newer outside focus', async () => {
  await page.viewport(390, 844);
  const history = setup();
  history.onOpenChange.mockImplementation((open: boolean) => history.setOpen(open));
  const editor = document.createElement('input');
  document.body.append(editor);
  try {
    history.onNew.mockImplementation(() => {
      expect(history.dialog().open).toBe(false);
      editor.focus();
    });
    await page.getByRole('button', { name: '发起新对话', exact: true }).click();
    await expect.poll(() => history.onNew.mock.calls.length).toBe(1);
    expect(document.activeElement).toBe(editor);
    history.dialog().dispatchEvent(new Event('close'));
    expect(document.activeElement).toBe(editor);
    expect(history.onNew).toHaveBeenCalledTimes(1);
  } finally { editor.remove(); }
});

it('registration: metadata refresh keeps owner but remount and A-B-A mint distinct owners', () => {
  const observed: object[] = [];
  const conversations: readonly Conversation[] = [];
  const noop = () => undefined;
  function Owner({ id, title }: { id: string; title: string }) {
    useMobileConversationOwner({ track: { id, title, areaId: 'area' }, conversations, selectedConversationId: null,
      onNew: noop, onClose: noop, onOpen: noop, conversationOpen: false });
    return null;
  }
  function Probe() {
    const scope = useMobileConversationScope();
    if (scope !== null) observed.push(scope.owner);
    return null;
  }
  const tree = (id: string, title: string, key: string) => <MobileConversationProvider><Owner key={key} id={id} title={title} /><Probe /></MobileConversationProvider>;
  const view = render(tree('A', 'First', 'mount-1'));
  const initial = observed.at(-1);
  view.rerender(tree('A', 'Renamed', 'mount-1'));
  expect(observed.at(-1)).toBe(initial);
  view.rerender(tree('A', 'Renamed', 'mount-2'));
  const remounted = observed.at(-1);
  expect(remounted).not.toBe(initial);
  view.rerender(tree('B', 'Other', 'mount-2'));
  const other = observed.at(-1);
  expect(other).not.toBe(remounted);
  view.rerender(tree('A', 'Back', 'mount-2'));
  expect(observed.at(-1)).not.toBe(remounted);
  expect(observed.at(-1)).not.toBe(other);
});

it.each(['parent still open', 'native still modal'] as const)('lifecycle-only: refuses action while %s', async phase => {
  await page.viewport(390, 844);
  const history = setup();
  await expect.poll(() => history.dialog().open).toBe(true);
  history.admit();
  if (phase === 'parent still open') await nativeClose(history.dialog());
  else {
    history.setOpen(false);
    history.dialog().dispatchEvent(new Event('close'));
    expect(history.onNew).not.toHaveBeenCalled();
    await nativeClose(history.dialog());
  }
  expect(history.onNew).not.toHaveBeenCalled();
});
