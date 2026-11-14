import '../../styles/entry.css';
import { AREA, setup } from './mobile-browser-fixture.tsx';
import { createReadReceiptsKernel } from '../providers/read-receipts-fixture.tsx';
import { cleanup } from '@testing-library/react';
import { createBrowserHistory } from '@tanstack/react-router';
import { afterEach, describe, expect, it } from 'vitest';
import { page, userEvent } from 'vitest/browser';
import type { ApiRequest } from '../../../../core/api/types.ts';

afterEach(cleanup);

describe('creating an Area after navigation changes', () => {
  it.each(['Back', 'same-URL push', 'same-URL replace'] as const)('keeps the current history entry after %s during creation', async (navigation) => {
    await page.viewport(390, 844);
    // Native Back must not traverse the browser runner's own history.
    const host = window.open(new URL('../router/native-history-fixture.html', import.meta.url).href, '_blank');
    if (host === null) throw new Error('Native history test requires an independent window');
    let releaseCreate!: () => void;
    const createHeld = new Promise<void>((resolve) => { releaseCreate = resolve; });
    try {
      await expect.poll(() => host.location.href.includes('native-history-fixture.html') && host.document.readyState === 'complete').toBe(true);
      host.history.replaceState({}, '', '/next/track/w1');
      const history = createBrowserHistory({ window: host });
      const receipts = createReadReceiptsKernel();
      const reply = receipts.reply;
      let creating = false;
      let areaReads = 0;
      receipts.reply = async (request) => {
        if (request.path === '/api/areas' && request.method === 'POST') {
          creating = true;
          await createHeld;
        }
        return reply(request);
      };
      const router = setup('/track/w1', AREA.name, (request) => {
        if (request.path === '/api/areas' && request.method === 'GET') areaReads++;
      }, receipts, history);
      try {
        await page.getByRole('button', { name: 'Switch track, Responsive mobile UI', exact: true }).click();
        await page.getByRole('button', { name: 'New track', exact: true }).click();
        await expect.poll(() => router.state.location.pathname).toBe('/area/c1/new');
        await page.getByRole('button', { name: 'Switch area, Product', exact: true }).click();
        await page.getByRole('menuitem', { name: 'New area', exact: true }).click();
        await page.getByRole('textbox', { name: /^Name/ }).fill('Research');
        await page.getByRole('button', { name: 'Create area', exact: true }).click();
        await expect.poll(() => creating).toBe(true);
        const readsBeforeSuccess = areaReads;
        if (navigation === 'Back') host.history.back();
        else if (navigation === 'same-URL push') history.push(history.location.href);
        else history.replace(history.location.href);
        const destination = navigation === 'Back' ? '/track/w1' : '/area/c1/new';
        await expect.poll(() => router.state.status === 'idle' && router.state.location.pathname === destination).toBe(true);
        history.flush();
        const entry = history.location.state.__TSR_key;
        releaseCreate();
        await expect.element(page.getByRole('dialog', { name: 'New area', exact: true })).not.toBeInTheDocument();
        await expect.poll(() => areaReads).toBeGreaterThan(readsBeforeSuccess);
        expect(router.state.location.pathname).toBe(destination);
        expect(history.location.state.__TSR_key).toBe(entry);
        await page.getByRole('button', { name: 'Open workspace' }).click();
        await expect.element(page.getByRole('button', { name: 'Research', exact: true })).toBeVisible();
      } finally { releaseCreate(); cleanup(); history.destroy(); }
    } finally { releaseCreate(); host.close(); }
  });
});

describe('creating an Area before its list is available', () => {
  it('opens an editable created Area draft before either Area list read settles', async () => {
    await page.viewport(390, 844);
    let releaseReads!: () => void;
    const readsHeld = new Promise<void>((resolve) => { releaseReads = resolve; });
    const receipts = createReadReceiptsKernel();
    const reply = receipts.reply;
    let reads = 0;
    const created = { ...AREA, id: 'c3', name: 'Research', default_template_id: 'saved', default_cwd: '/srv/research' };
    receipts.reply = async (request) => {
      if (request.path === '/api/areas' && request.method === 'GET') {
        reads++;
        await readsHeld;
        return { status: 200, statusText: 'OK', body: [AREA, created] };
      }
      if (request.path === '/api/areas' && request.method === 'POST') {
        return { status: 201, statusText: 'Created', body: created };
      }
      if (request.path === '/api/track-templates') {
        return { status: 200, statusText: 'OK', body: [{ id: 'saved', title: 'Saved workflow', description: null, tasks: [], availability: { status: 'ready' } }] };
      }
      return reply(request);
    };
    const router = setup('/track/w1', AREA.name, () => undefined, receipts);
    try {
      await page.getByRole('button', { name: 'Open workspace' }).click();
      await page.getByRole('button', { name: 'New area', exact: true }).click();
      await page.getByRole('textbox', { name: /^Name/ }).fill('Research');
      await page.getByRole('button', { name: 'Create area', exact: true }).click();
      await expect.poll(() => router.state.location.pathname).toBe('/area/c3/new');
      await expect.poll(() => reads).toBeGreaterThanOrEqual(2);
      await page.getByRole('button', { name: 'Switch area, Research', exact: true }).findElement();
      const composer = page.getByRole('combobox', { name: 'What this track should do' });
      await composer.fill('Keep this draft while Areas load');
      await expect.element(page.getByRole('status').getByText('Loading Area…')).toBeVisible();
      expect(page.getByRole('main').getByRole('alert').query()).toBeNull();
      await expect.element(page.getByRole('button', { name: 'Create track', exact: true })).toBeDisabled();
      await page.getByRole('button', { name: 'Track options' }).click();
      await expect.element(page.getByRole('button', { name: 'Repo: research (/srv/research)' })).toBeVisible();
      // A saved Area template default does not expose a proactive template picker.
      await expect.element(page.getByRole('button', { name: /^Template:/ })).not.toBeInTheDocument();
      await page.getByRole('button', { name: 'Track options' }).click();
      releaseReads();
      await expect.element(page.getByRole('button', { name: 'Create track', exact: true })).toBeEnabled();
      await expect.element(composer).toHaveTextContent('Keep this draft while Areas load');
    } finally {
      releaseReads();
    }
  });

  it('preserves the created draft through a failed list read and its retry', async () => {
    await page.viewport(390, 844);
    let releaseReads!: () => void;
    const readsHeld = new Promise<void>((resolve) => { releaseReads = resolve; });
    let failList = true;
    const requests: ApiRequest[] = [];
    const receipts = createReadReceiptsKernel();
    const reply = receipts.reply;
    receipts.reply = async (request) => {
      if (request.path === '/api/areas' && request.method === 'GET') {
        await readsHeld;
        if (failList) return { status: 500, statusText: 'Error', body: { error: 'unavailable', code: 'internal' } };
      }
      return reply(request);
    };
    const router = setup('/track/w1', AREA.name, (request) => requests.push(request), receipts);
    try {
      await page.getByRole('button', { name: 'Open workspace' }).click();
      await page.getByRole('button', { name: 'New area', exact: true }).click();
      await page.getByRole('textbox', { name: /^Name/ }).fill('Research');
      await page.getByRole('button', { name: 'Create area', exact: true }).click();
      await expect.poll(() => router.state.location.pathname).toBe('/area/c3/new');
      const composer = page.getByRole('combobox', { name: 'What this track should do' });
      await composer.fill('Keep my research plan');
      releaseReads();
      await expect.element(page.getByRole('main').getByRole('alert')).toBeVisible();
      await expect.element(composer).toHaveTextContent('Keep my research plan');
      await composer.fill('Keep my research plan after retry');
      await userEvent.keyboard('{Enter}');
      await expect.element(page.getByRole('button', { name: 'Create track', exact: true })).toBeDisabled();
      expect(requests.some(request => request.path === '/api/tracks' && request.method === 'POST')).toBe(false);
      failList = false;
      await page.getByRole('button', { name: 'Open workspace' }).click();
      await page.getByRole('button', { name: 'Retry', exact: true }).click();
      await page.getByRole('button', { name: 'Back to workspace', exact: true }).click();
      await expect.element(page.getByRole('button', { name: 'Create track', exact: true })).toBeEnabled();
      await expect.element(composer).toHaveTextContent('Keep my research plan after retry');
      expect(page.getByRole('main').getByRole('alert').query()).toBeNull();
    } finally {
      releaseReads();
    }
  });
});
