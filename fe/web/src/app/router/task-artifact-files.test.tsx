import { act, cleanup, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { createTaskFileFixture } from './task-artifact-file-fixture.ts';
import { mountIsolatedRetryRoute } from './isolated-task-retry-fixture.tsx';

let blobs: Blob[];
let downloads: { href: string; name: string }[];
let revoked: string[];
let oldCreate: PropertyDescriptor | undefined;
let oldRevoke: PropertyDescriptor | undefined;
beforeEach(() => {
  blobs = []; downloads = []; revoked = [];
  oldCreate = Object.getOwnPropertyDescriptor(URL, 'createObjectURL');
  oldRevoke = Object.getOwnPropertyDescriptor(URL, 'revokeObjectURL');
  Object.defineProperty(URL, 'createObjectURL', { configurable: true, value: (blob: Blob) => {
    blobs.push(blob); return `blob:task-file-${blobs.length}`;
  } });
  Object.defineProperty(URL, 'revokeObjectURL', { configurable: true, value: (url: string) => { revoked.push(url); } });
  vi.spyOn(HTMLAnchorElement.prototype, 'click').mockImplementation(function (this: HTMLAnchorElement) {
    downloads.push({ href: this.href, name: this.download });
  });
});
afterEach(() => {
  cleanup(); vi.restoreAllMocks();
  if (oldCreate) Object.defineProperty(URL, 'createObjectURL', oldCreate); else Reflect.deleteProperty(URL, 'createObjectURL');
  if (oldRevoke) Object.defineProperty(URL, 'revokeObjectURL', oldRevoke); else Reflect.deleteProperty(URL, 'revokeObjectURL');
});

async function openReport() {
  await userEvent.click(await screen.findByText('Reference'));
  await userEvent.click(document.querySelector('[data-nc-task-state] > summary')!);
  await screen.findByText('Reported files are available.');
}
function fileAction(refs: readonly string[], index: number) {
  return within(screen.getByText(refs[index]).closest('li')!).getByRole('button', { name: `View file ${index + 1}` });
}
function blobBytes(blob: Blob): Promise<Uint8Array> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => resolve(new Uint8Array(reader.result as ArrayBuffer));
    reader.onerror = () => reject(new Error(reader.error?.message ?? 'Could not read test Blob.'));
    reader.readAsArrayBuffer(blob);
  });
}

it('reads only the selected scoped file, escapes HTML, downloads exact bytes and releases it on close', async () => {
  const fixture = createTaskFileFixture();
  const app = mountIsolatedRetryRoute(fixture.transport);
  try {
    await openReport();
    expect(fixture.fileRequests).toHaveLength(0);
    expect(screen.getByText('javascript:alert(1)').tagName).toBe('LI');
    expect(document.querySelector('a[href="javascript:alert(1)"]')).toBeNull();
    await userEvent.click(fileAction(fixture.refs, 0));
    const preview = await screen.findByText(/Hello, 世界!/, { selector: 'pre' });
    expect(preview.textContent).toBe(fixture.text);
    expect(document.querySelector('img[src="x"]')).toBeNull();
    expect(downloads).toHaveLength(0);
    expect(fixture.fileRequests).toHaveLength(1);
    expect(fixture.fileRequests[0].path).toBe(`/api/tracks/w1/tasks/${fixture.base.taskKey}/attempts/${fixture.base.queued.attempt_id}/artifacts/0`);
    expect(fixture.fileRequests[0].credentials).toBe('include');
    expect(blobs).toHaveLength(1);
    expect(blobs[0].type).toBe('application/octet-stream');
    await userEvent.click(screen.getByRole('button', { name: 'Download file' }));
    expect(downloads).toEqual([{ href: 'blob:task-file-1', name: '结果.html' }]);
    expect(Array.from(await blobBytes(blobs[0]))).toEqual(Array.from(new TextEncoder().encode(fixture.text)));
    expect(document.querySelector('a[download]')).toBeNull();
    await userEvent.click(screen.getByRole('button', { name: 'Close' }));
    expect(revoked).toEqual(['blob:task-file-1']);
    expect(screen.queryByRole('dialog')).toBeNull();
  } finally { app.dispose(); }
});

it.each([1, 2])('keeps binary/empty file index %i downloadable without executing content', async (index) => {
  const fixture = createTaskFileFixture();
  const app = mountIsolatedRetryRoute(fixture.transport);
  try {
    await openReport();
    await userEvent.click(fileAction(fixture.refs, index));
    await screen.findByText(index === 1 ? 'This file has no plain-text preview. You can download it.' : 'Empty file.');
    await userEvent.click(screen.getByRole('button', { name: 'Download file' }));
    expect(downloads[0].name).toBe(index === 1 ? 'binary.bin' : '空.txt');
    expect(await blobBytes(blobs[0])).toEqual(index === 1 ? new Uint8Array([255, 1, 2]) : new Uint8Array());
  } finally { app.dispose(); }
  expect(revoked).toEqual(['blob:task-file-1']);
});

it.each([400, 401, 403, 404, 409, 413])('shows server %i failure and explicitly retries the unchanged file identity', async (status) => {
  const fixture = createTaskFileFixture();
  fixture.response(0, { status, statusText: 'Unavailable', body: { error: 'The reported file is not available now.', code: 'unavailable' } });
  const app = mountIsolatedRetryRoute(fixture.transport);
  try {
    await openReport();
    await userEvent.click(fileAction(fixture.refs, 0));
    await screen.findByText('Could not open file: The reported file is not available now.');
    expect(blobs).toHaveLength(0);
    expect(screen.queryByRole('button', { name: 'Download file' })).toBeNull();
    fixture.response(0, fixture.file(0, new TextEncoder().encode('Now available.'), 'ready.txt'));
    await userEvent.click(screen.getByRole('button', { name: 'Retry file' }));
    await screen.findByText('Now available.', { selector: 'pre' });
    expect(fixture.fileRequests).toHaveLength(2);
    expect(fixture.fileRequests[1].path).toBe(fixture.fileRequests[0].path);
    expect(fixture.fileRequests[0].signal?.aborted).toBe(true);
  } finally { app.dispose(); }
  expect(revoked).toEqual(['blob:task-file-1']);
});

it('rejects wrong response identity without creating a file resource', async () => {
  const fixture = createTaskFileFixture();
  fixture.response(0, fixture.file(1, new Uint8Array([42]), 'wrong.txt', 'different-attempt'));
  const app = mountIsolatedRetryRoute(fixture.transport);
  try {
    await openReport();
    await userEvent.click(fileAction(fixture.refs, 0));
    await screen.findByText(/Could not open file: The file response could not be verified/);
    expect(blobs).toHaveLength(0);
    expect(screen.queryByRole('button', { name: 'Download file' })).toBeNull();
  } finally { app.dispose(); }
});

it('aborts a closed file read and ignores its late response', async () => {
  const fixture = createTaskFileFixture();
  fixture.hold();
  const app = mountIsolatedRetryRoute(fixture.transport);
  try {
    await openReport();
    await userEvent.click(fileAction(fixture.refs, 0));
    await screen.findByText('Loading file…');
    await waitFor(() => expect(fixture.fileRequests).toHaveLength(1));
    await userEvent.click(screen.getByRole('button', { name: 'Close' }));
    expect(fixture.fileRequests[0].signal?.aborted).toBe(true);
    await act(() => { fixture.release(fixture.file(0, new Uint8Array([42]), 'late.txt')); return Promise.resolve(); });
    expect(blobs).toHaveLength(0);
    expect(screen.queryByRole('dialog')).toBeNull();
  } finally { app.dispose(); }
});

it('bounds the preview and replaces resources across files, history and a cold remount', async () => {
  const fixture = createTaskFileFixture();
  let app = mountIsolatedRetryRoute(fixture.transport);
  try {
    await openReport();
    await userEvent.click(fileAction(fixture.refs, 4));
    await screen.findByText('Preview truncated to 65,536 characters. Download the file for its full contents.');
    expect(within(screen.getByRole('dialog')).getByText(/^字+$/, { selector: 'pre' }).textContent?.length).toBe(65_536);
    await userEvent.click(screen.getByRole('button', { name: 'Close' }));
    expect(revoked).toEqual(['blob:task-file-1']);
    await userEvent.click(screen.getByText('Attempt history (2)'));
    await userEvent.click(screen.getByText('Attempt 1 · Completed'));
    const historic = screen.getByText('history.txt').closest('li')!;
    await userEvent.click(within(historic).getByRole('button', { name: 'View file 1' }));
    await screen.findByText('Historical bytes.', { selector: 'pre' });
    expect(fixture.fileRequests.at(-1)?.path).toContain(`/${fixture.base.first.attempt_id}/artifacts/0`);
    app.dispose();
    expect(revoked).toEqual(['blob:task-file-1', 'blob:task-file-2']);
    const before = fixture.fileRequests.length;
    app = mountIsolatedRetryRoute(fixture.transport);
    await openReport();
    expect(fixture.fileRequests).toHaveLength(before);
    await userEvent.click(fileAction(fixture.refs, 0));
    await screen.findByText(/Hello, 世界!/, { selector: 'pre' });
    expect(fixture.fileRequests).toHaveLength(before + 1);
    expect(screen.getAllByRole('dialog')).toHaveLength(1);
  } finally { app.dispose(); }
  expect(revoked).toEqual(['blob:task-file-1', 'blob:task-file-2', 'blob:task-file-3']);
});

it('keeps valid UTF-8 containing NUL out of the preview without altering download bytes', async () => {
  const fixture = createTaskFileFixture();
  fixture.response(0, fixture.file(0, new Uint8Array([65, 0, 66]), 'nul.txt'));
  const app = mountIsolatedRetryRoute(fixture.transport);
  try {
    await openReport();
    await userEvent.click(fileAction(fixture.refs, 0));
    await screen.findByText('This file has no plain-text preview. You can download it.');
    await userEvent.click(screen.getByRole('button', { name: 'Download file' }));
    expect(Array.from(await blobBytes(blobs[0]))).toEqual([65, 0, 66]);
  } finally { app.dispose(); }
});
