import { cleanup } from '@testing-library/react';
import { page, userEvent } from 'vitest/browser';
import { afterEach, expect, it, vi } from 'vitest';
import { createTaskFileFixture } from './task-artifact-file-fixture.ts';
import { mountIsolatedRetryRoute } from './isolated-task-retry-fixture.tsx';
import '../../styles/entry.css';

afterEach(() => { cleanup(); vi.restoreAllMocks(); });

it('opens escaped text and exact Blob downloads, then releases each file on close or route unmount', async () => {
  const fixture = createTaskFileFixture();
  const objects: { url: string; blob: Blob }[] = [];
  const downloads: { url: string; name: string }[] = [];
  const realCreate = URL.createObjectURL.bind(URL);
  vi.spyOn(URL, 'createObjectURL').mockImplementation((blob) => {
    if (!(blob instanceof Blob)) throw new Error('Expected the task file Blob.');
    const url = realCreate(blob); objects.push({ url, blob }); return url;
  });
  const revoke = vi.spyOn(URL, 'revokeObjectURL');
  // Actual saveAs/byte comparison belongs to the parent's Playwright harness.
  vi.spyOn(HTMLAnchorElement.prototype, 'click').mockImplementation(function (this: HTMLAnchorElement) {
    downloads.push({ url: this.href, name: this.download });
  });
  const app = mountIsolatedRetryRoute(fixture.transport);
  try {
    await page.viewport(1280, 900);
    await page.getByText('Reference', { exact: true }).click();
    await userEvent.click(document.querySelector('[data-nc-task-state] > summary')!);
    await expect.element(page.getByText('Reported files are available.', { exact: true })).toBeVisible();
    expect(fixture.fileRequests).toHaveLength(0);
    expect(document.querySelector('a[href="javascript:alert(1)"]')).toBeNull();
    await page.getByRole('button', { name: 'View file 1', exact: true }).click();
    await expect.element(page.getByRole('button', { name: 'Download file', exact: true })).toBeVisible();
    const preview = document.querySelector('[role="dialog"] pre')!;
    expect(preview.textContent).toBe(fixture.text);
    expect(document.querySelector('img[src="x"]')).toBeNull();
    expect(downloads).toHaveLength(0);
    await Promise.all(document.querySelector('[role="dialog"]')!.getAnimations().map((animation) => animation.finished));
    await page.screenshot({ path: '__screenshots__/issue-1501-file-text.png' });
    await page.getByRole('button', { name: 'Download file', exact: true }).click();
    expect(downloads).toEqual([{ url: objects[0].url, name: '结果.html' }]);
    expect(objects[0].blob.type).toBe('application/octet-stream');
    expect(new Uint8Array(await objects[0].blob.arrayBuffer())).toEqual(new TextEncoder().encode(fixture.text));
    expect(document.querySelector('a[download]')).toBeNull();
    await page.getByRole('button', { name: 'Close', exact: true }).click();
    expect(revoke).toHaveBeenCalledWith(objects[0].url);
    await page.getByRole('button', { name: 'View file 2', exact: true }).click();
    await expect.element(page.getByText('This file has no plain-text preview. You can download it.', { exact: true })).toBeVisible();
    await page.getByRole('button', { name: 'Download file', exact: true }).click();
    expect(downloads[1]).toEqual({ url: objects[1].url, name: 'binary.bin' });
    expect(new Uint8Array(await objects[1].blob.arrayBuffer())).toEqual(new Uint8Array([255, 1, 2]));
    await Promise.all(document.querySelector('[role="dialog"]')!.getAnimations().map((animation) => animation.finished));
    await page.screenshot({ path: '__screenshots__/issue-1501-file-binary.png' });
    await page.getByRole('button', { name: 'Close', exact: true }).click();
    expect(revoke).toHaveBeenCalledWith(objects[1].url);
    await page.getByRole('button', { name: 'View file 3', exact: true }).click();
    await expect.element(page.getByText('Empty file.', { exact: true })).toBeVisible();
    await page.getByRole('button', { name: 'Download file', exact: true }).click();
    expect(downloads[2]).toEqual({ url: objects[2].url, name: '空.txt' });
    expect((await objects[2].blob.arrayBuffer()).byteLength).toBe(0);
    expect(fixture.fileRequests.map((request) => request.path)).toEqual([0, 1, 2].map((index) =>
      `/api/tracks/w1/tasks/${fixture.base.taskKey}/attempts/${fixture.base.queued.attempt_id}/artifacts/${index}`));
  } finally { app.dispose(); }
  expect(revoke.mock.calls.map(([url]) => url)).toEqual(objects.map(({ url }) => url));
});
