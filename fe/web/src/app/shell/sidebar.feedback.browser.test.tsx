import { AreaEditorForm } from '../../features/area/editor/public.tsx';
import { useRef } from 'react';
import { NativeReportView } from '../../features/report/native/public.tsx';
import { RecipeEditor } from '../../features/report/recipe/public.tsx';
import { NewTrackForm } from '../../features/area/new-track/public.tsx';
import { AddCardMenu, NewCardForm } from '../../features/track/new-card/public.tsx';
import type { CardAddMenuEntry } from '../../systems/cards/public.ts';
import { nativeViewPayloadSchema } from '../../../../core/domain/report-view.ts';
import source from '../../../../../test-data/native-view-v1.json?raw';
import { Dialog, type DialogOrigin } from '../../ui/dialog/public.tsx';
import { useState } from '../../ui/state/public.ts';
import { renderPage } from '../../features/track/page/test-fixtures.tsx';
// #2175-17: a failed sidebar write (here, a pin the server refused) is said in the rail as the Astryx error banner, in
// a real engine, light and dark.
import { cleanup, render, screen } from '@testing-library/react';
import { page, userEvent } from 'vitest/browser';
import { afterEach, expect, it, vi } from 'vitest';

import type { Area } from '../../../../core/domain/area.ts';
import { ApiError } from '../../../../core/domain/failure-class.ts';
import { NEUTRAL_ACTIVITY, type Track } from '../../../../core/domain/track.ts';
import '../../styles/entry.css';
import { ThemeProvider } from '../theme/public.tsx';
import { createUiPreferences, UiPreferencesProvider } from '../providers/ui-preferences.tsx';
import { SidebarFixture as Sidebar } from './sidebar-fixture.tsx';
import styles from './shell.module.css';
import { readReceiptsFixture } from '../providers/read-receipts-fixture.tsx';

const receipts = readReceiptsFixture();

afterEach(() => { cleanup(); delete document.documentElement.dataset.theme; });

it.each(['light', 'dark'] as const)('says a refused pin in the rail as an error banner (%s)', async (theme) => {
  await page.viewport(1280, 720);
  const area: Area = { id: 'work', name: 'Work', color: '#5B8DEF', sort: 1, kind: 'user',
    defaultTemplateId: null, defaultCwd: null, createdAt: 1, updatedAt: 1 };
  const tracks: Track[] = [{ id: 'next', title: 'Next task', areaId: area.id, sort: 1, cwd: '/tmp', isolation: 'none', agentCwd: '/tmp',
    pinnedAt: null, closedAt: null, createdAt: 1, updatedAt: 10, ...NEUTRAL_ACTIVITY, activityAt: null }];
  const onSetPinned = vi.fn(() => Promise.reject(new ApiError({ kind: 'http', status: 409, code: 'conflict',
    message: 'A managed track cannot be pinned.' })));
  render(receipts.wrap(<UiPreferencesProvider preferences={createUiPreferences()}>
    <ThemeProvider storage={{ getItem: () => theme, setItem: () => undefined }}>
      <div className={`${styles.shell} ${styles.shellExpanded}`} style={{ blockSize: '100dvh' }}>
        <Sidebar areas={[area]} tracksByArea={new Map([[area.id, tracks]])} tracks={tracks}
          currentPath="/" onGo={vi.fn()} onRequestCreateArea={vi.fn()} onRequestEditArea={vi.fn()}
          onDeleteArea={vi.fn()} onNewTrack={vi.fn()} onSetPinned={onSetPinned} onDeleteTrack={vi.fn()}
          onOpenSettings={vi.fn()} onOpenPlugins={vi.fn()} onSignOut={vi.fn()}
          collapsed={false} onToggleCollapsed={vi.fn()} />
        <main />
      </div>
    </ThemeProvider>
  </UiPreferencesProvider>));

  await page.getByRole('button', { name: /^Track Next task/ }).hover();
  await page.getByRole('button', { name: 'Actions for track Next task' }).click();
  await page.getByRole('menuitem', { name: 'Pin globally' }).click();
  const alert = page.getByRole('alert');
  await expect.element(alert).toBeVisible();
  await expect.element(alert.getByText('A managed track cannot be pinned.')).toBeVisible();
  await page.screenshot({ path: `test-results/operation-feedback-${theme}.png` });

  /* The banner treatment: a filled error surface with its glyph and a Dismiss, not bare text on the rail. */
  const element = alert.element();
  const filled = [element, ...element.querySelectorAll('*')].some((node) => getComputedStyle(node).backgroundColor !== 'rgba(0, 0, 0, 0)');
  expect(filled).toBe(true);
  expect(element.querySelector('svg')).not.toBeNull();
  await alert.getByRole('button', { name: /^Dismiss/ }).click();
  await expect.element(page.getByRole('alert')).not.toBeInTheDocument();
});

// Cross-feature Dialog entry composition belongs to app; each entry is the real component.

const tick = () => new Promise<void>(resolve => requestAnimationFrame(() => resolve()));
async function anchorIs(source: HTMLElement, name?: string) {
  const node = screen.getByRole('dialog', name === undefined ? undefined : { name });
  const animation = node.getAnimations()[0];
  expect(animation).toBeDefined(); await animation.ready;
  animation.pause(); animation.currentTime = 0; await tick();
  const frame = node.parentElement!.getBoundingClientRect();
  const rect = source.getBoundingClientRect();
  const point = { x: rect.left + rect.width / 2, y: rect.top + rect.height / 2 };
  const m = new DOMMatrix(getComputedStyle(node).transform);
  const p = new DOMPoint(point.x - frame.left, point.y - frame.top).matrixTransform(m);
  expect(frame.left + p.x).toBeCloseTo(point.x, 3);
  expect(frame.top + p.y).toBeCloseTo(point.y + .56, 3);
  animation.play(); await expect.poll(() => node.getAnimations().length).toBe(0);
  return node;
}
it('opens actual Native fullscreen from the unpressed action and keeps its inspection tree', async () => {
  const payload = nativeViewPayloadSchema.parse((JSON.parse(source) as { valid: unknown }).valid);
  render(<NativeReportView payload={payload} />);
  const action = screen.getByRole('button', { name: /^放大查看/ });
  await userEvent.click(action);
  await anchorIs(action.parentElement!);
  await page.screenshot({ path: 'test-results/dialog-native.png' });
  await page.getByRole('dialog').getByRole('button', { name: 'Close', exact: true }).click();
  expect(screen.queryByRole('dialog')).toBeNull();
});
it('anchors actual Recipe delete and cancels without calling its business port', async () => {
  const remove = vi.fn(() => Promise.resolve());
  render(<RecipeEditor recipe={{ id: 'r', title: 'Ship', body: 'Checklist', revision: 1, created_at: 1, updated_at: 1 }}
    theme="light" onWrite={vi.fn()} onDelete={remove} onClose={vi.fn()} onCreated={null} />);
  const action = screen.getByRole('button', { name: 'Delete' });
  await userEvent.click(action);
  await anchorIs(action.parentElement!);
  await expect.element(page.getByRole('button', { name: 'Cancel', exact: true })).toHaveFocus();
  await page.screenshot({ path: 'test-results/dialog-recipe-delete.png' });
  await userEvent.keyboard('{Escape}');
  expect(remove).not.toHaveBeenCalled();
});
function CardEntry() {
  const [origin, setOrigin] = useState<DialogOrigin | null>(null);
  const initial = useRef<HTMLInputElement | null>(null);
  const entry: CardAddMenuEntry = { type: 'tool', label: 'Tool', fields: [{ kind: 'text', key: 'title', label: 'Title' }] };
  return <><AddCardMenu entries={[entry]} onSelect={(_entry, _trigger, origin) => setOrigin(origin)} />
    <Dialog open={origin !== null} origin={origin ?? { kind: 'surface-center' }} title="New tool" initialFocusRef={initial} onClose={() => setOrigin(null)}>
      <NewCardForm entry={entry} submitting={false} error={null} onRetry={null} listDirectory={() => Promise.resolve({ path: '/', parent: null, entries: [] })}
        firstFieldRef={initial} onCancel={() => setOrigin(null)} onSubmit={vi.fn()} />
    </Dialog></>;
}
it('keeps actual AddCardMenu focus trigger separate from the NewCard source', async () => {
  render(<CardEntry />);
  const action = screen.getByRole('button', { name: 'Add card' });
  await userEvent.click(action);
  await page.getByRole('menuitem', { name: 'Tool' }).click();
  await anchorIs(action.parentElement!);
  await expect.element(page.getByRole('textbox', { name: 'Title' })).toHaveFocus();
  await page.screenshot({ path: 'test-results/dialog-card.png' });
});
it('opens the actual NewTrack folder picker from its inline folder host', async () => {
  render(<NewTrackForm loadTemplate={vi.fn()} submitting={false} error={null} templates={[]} templatesLoaded
    initialTemplateId={null} initialCwd={null} onManageRecipes={vi.fn()}
    listDirectory={() => Promise.resolve({ path: '/', parent: null, entries: [] })} onSubmit={vi.fn()} />);
  const folder = screen.getByRole('button', { name: /^Folder:/ });
  await userEvent.click(folder);
  expect(folder.parentElement!.getAnimations()).toHaveLength(0);
  const node = screen.getByRole('dialog');
  expect(node.getAnimations()).toHaveLength(1);
  await expect.poll(() => node.getAnimations().length).toBe(0);
  await page.screenshot({ path: 'test-results/dialog-folder.png' });
});
it('keeps actual Track desktop menu focus return out of an open delete modal', async () => {
  await page.viewport(1440, 900);
  renderPage();
  await page.getByRole('button', { name: 'Track actions for Alpha' }).click();
  await page.getByRole('menuitem', { name: 'Delete track' }).click();
  await expect.element(page.getByRole('button', { name: 'Cancel', exact: true })).toHaveFocus();
  await tick();
  expect(screen.getByRole('dialog').contains(document.activeElement)).toBe(true);
  await page.screenshot({ path: 'test-results/dialog-track-delete.png' });
});

function AreaEntry({ empty }: { empty: boolean }) {
  const areas: Area[] = empty ? [] : [{ id: 'work', name: 'Work', color: '#5B8DEF', sort: 1, kind: 'user',
    defaultTemplateId: null, defaultCwd: null, createdAt: 1, updatedAt: 1 }];
  const [origin, setOrigin] = useState<DialogOrigin | null>(null);
  const input = useRef<HTMLInputElement | null>(null);
  return <>{readReceiptsFixture().wrap(<UiPreferencesProvider preferences={createUiPreferences()}>
    <Sidebar areas={areas} tracksByArea={new Map()} tracks={[]} currentPath="/" onGo={vi.fn()}
      onRequestCreateArea={setOrigin} onRequestEditArea={vi.fn()} onDeleteArea={vi.fn()} onNewTrack={vi.fn()}
      onSetPinned={vi.fn()} onDeleteTrack={vi.fn()} onOpenSettings={vi.fn()} onOpenPlugins={vi.fn()} onSignOut={vi.fn()}
      collapsed={false} onToggleCollapsed={vi.fn()} />
  </UiPreferencesProvider>)}
    <Dialog origin={origin ?? { kind: 'surface-center' }} open={origin !== null} title="New area" onClose={() => setOrigin(null)} initialFocusRef={input}>
      <AreaEditorForm initial={{ name: '', defaultTemplateId: null, defaultCwd: null }} submitting={false} locked={false}
        cancelLabel="Cancel" error={null} templates={[]} templatesLoaded templatesError={null}
        listDirectory={() => Promise.resolve({ path: '/', parent: null, entries: [] })}
        nameInputRef={input} submitLabel="Create area" onCancel={() => setOrigin(null)} onSubmit={vi.fn()} />
    </Dialog></>;
}
it.each([false, true])('uses the actual Sidebar creation source with the Area form and immediate child stack (empty=%s)', async empty => {
  await page.viewport(1200, 800);
  render(<AreaEntry empty={empty} />);
  if (empty) {
    expect(screen.getByRole('heading', { name: '还没有 Area' })).toBeTruthy();
    expect(screen.queryByRole('button', { name: 'Create your first area' })).toBeNull();
  }
  const action = screen.getByRole('button', { name: 'New area' });
  await userEvent.click(action);
  const node = await anchorIs(action.parentElement!);
  const name = page.getByRole('textbox', { name: 'Name' });
  await name.fill('Work draft');
  await page.getByRole('button', { name: /^Default folder:/ }).click();
  await expect.element(page.getByRole('dialog', { name: 'Choose a directory' })).toBeVisible();
  expect(screen.getByRole('dialog')).toBe(node);
  await userEvent.keyboard('{Escape}');
  await expect.element(name).toHaveValue('Work draft');
  await page.screenshot({ path: `test-results/dialog-area-${empty ? 'empty' : 'icon'}.png` });
});
