import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { page } from 'vitest/browser';
import { afterEach, expect, it, vi } from 'vitest';
import '../../../styles/entry.css';
import { ReportDocument } from './public.tsx';

afterEach(() => { cleanup(); document.documentElement.removeAttribute('data-theme'); });

it.each(['light', 'dark'])('renders and opens SVG and PNG evidence at phone width in %s theme', async (theme) => {
  await page.viewport(390, 844);
  document.documentElement.setAttribute('data-theme', theme);
  const svg = 'data:image/svg+xml,' + encodeURIComponent('<svg xmlns="http://www.w3.org/2000/svg" width="1600" height="800"><rect width="1600" height="800" fill="#789abc"/><text x="40" y="80">Architecture</text></svg>');
  const canvas = document.createElement('canvas');
  canvas.width = 1600; canvas.height = 800;
  const drawing = canvas.getContext('2d')!;
  drawing.fillStyle = '#789abc';
  drawing.fillRect(0, 0, canvas.width, canvas.height);
  const png = canvas.toDataURL('image/png');
  const files = { readFile: vi.fn(), rawUrl: vi.fn((path: string) => path.endsWith('.svg') ? svg : png) };
  const report = { summary: '', body: '![Diagram](docs/diagram.svg)\n\n![Screenshot](docs/screenshot.png)', blocks: null };
  const open = vi.fn();
  render(<ReportDocument report={report} empty={null} fileRoot="/repo" onOpenFileLink={open}
    linkPreview={{ trackId: 'track', report, files }} />);
  for (const name of ['Diagram', 'Screenshot']) {
    const image = await screen.findByRole<HTMLImageElement>('img', { name });
    await waitFor(() => { expect(image.naturalWidth).toBe(1600); });
    const box = image.getBoundingClientRect();
    const column = image.closest('[data-nc-report-reading]')!.getBoundingClientRect();
    expect(box.width).toBeGreaterThan(100);
    expect(box.width).toBeLessThanOrEqual(column.width);
    expect(box.right).toBeLessThanOrEqual(window.innerWidth);
    expect(Math.abs(box.width / box.height - 2)).toBeLessThan(0.02);
    expect(image.loading).toBe('lazy');
    await page.getByRole('button', { name }).click();
  }
  expect(open.mock.calls).toEqual([[{ path: 'docs/diagram.svg' }], [{ path: 'docs/screenshot.png' }]]);
  expect(files.readFile).not.toHaveBeenCalled();
  expect(document.querySelector('svg, object, iframe')).toBeNull();
});

it('fits the desktop prose measure and keeps linked image navigation on the outer link', async () => {
  await page.viewport(1280, 900);
  const report = { summary: '', body: '[![Diagram](diagram.svg)](notes.md)', blocks: null };
  const open = vi.fn();
  const files = { readFile: vi.fn(), rawUrl: () => 'data:image/svg+xml,' + encodeURIComponent('<svg xmlns="http://www.w3.org/2000/svg" width="1600" height="800"/>') };
  render(<ReportDocument report={report} empty={null} fileRoot="/repo" onOpenFileLink={open}
    linkPreview={{ trackId: 'track', report, files }} />);
  const image = await screen.findByRole<HTMLImageElement>('img', { name: 'Diagram' });
  await waitFor(() => { expect(image.naturalWidth).toBe(1600); });
  expect(image.getBoundingClientRect().width).toBeLessThanOrEqual(image.closest('[data-nc-report-reading]')!.getBoundingClientRect().width);
  expect(document.querySelector('button button, button a, a button')).toBeNull();
  await page.getByRole('button', { name: 'Diagram' }).click();
  expect(open).toHaveBeenCalledWith({ path: 'notes.md' });
});

// The load control belongs to an admitted HoverPreview. Exercise its native
// layout here; inline workspace resources and invalid paths remain DOM tests.
it.each([390, 1280])('keeps remote images opt-in until the load control is activated at %ipx', async width => {
  await page.viewport(width, 900);
  const report = { summary: '', body: '![Remote](https://example.com/pic.png)', blocks: null };
  const files = { readFile: vi.fn(), rawUrl: vi.fn() };
  const { container } = render(<ReportDocument report={report} empty={null} fileRoot="/repo"
    linkPreview={{ trackId: 'image-track', report, files }} />);
  expect(container.querySelector('img, iframe, object')).toBeNull();
  expect(files.rawUrl).not.toHaveBeenCalled();
  await page.getByRole('button', { name: 'Remote', exact: true }).click();
  const load = await screen.findByRole('button', { name: 'Load image' });
  const preview = screen.getByRole('dialog', { name: 'Preview: Remote' });
  expect(preview.closest('[inert], [aria-hidden="true"]')).toBeNull();
  expect(preview.closest('[popover]')?.matches(':popover-open')).toBe(true);
  expect(preview.getBoundingClientRect().width).toBeGreaterThan(0);
  expect(document.querySelector('img, iframe, object')).toBeNull();
  // Read the resource contract in the activation commit, before the external
  // server can answer with an unrelated load or network failure.
  fireEvent.click(load);
  const image = screen.getByRole('img', { name: 'Remote' });
  expect(image.getAttribute('src')).toBe('https://example.com/pic.png');
  expect(image.getAttribute('referrerpolicy')).toBe('no-referrer');
  expect(files.rawUrl).not.toHaveBeenCalled();
  expect(files.readFile).not.toHaveBeenCalled();
});
