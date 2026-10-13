import { cleanup, render, screen, waitFor } from '@testing-library/react';
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
