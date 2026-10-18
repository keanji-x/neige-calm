import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { ReportDocument } from '../document/public.tsx';
import { externalPreviewUrl } from './public.tsx';

afterEach(cleanup);

it('uses the scoped image port for workspace image previews', async () => {
  const files = { readFile: vi.fn(), rawUrl: vi.fn((path: string) => `/workspace/${path}`) };
  const report = { summary: '', body: '![local image](./assets/chart.png)', blocks: null };
  render(<ReportDocument report={report} empty={null} fileRoot="/repo"
    linkPreview={{ files, trackId: 't1', report }} onOpenFileLink={vi.fn()} />);
  const image = await screen.findByRole('img');
  fireEvent.pointerEnter(image.closest('button')!.parentElement!);
  expect(image.getAttribute('src')).toBe('/workspace/assets/chart.png');
  expect(files.rawUrl).toHaveBeenCalledWith('assets/chart.png');
});

describe('external URL contract', () => {
  it.each(['javascript:alert(1)', 'data:text/html,x', '//example.com', '/api/logout', 'https://u:p@example.com', 'https://example.com\\evil', 'https://example.com\n', 'file:///etc/passwd'])('refuses %s', (url) => {
    expect(externalPreviewUrl(url)).toBeNull();
  });
  it('admits an explicit HTTP(S) destination', () => { expect(externalPreviewUrl('https://example.com/path')).toBe('https://example.com/path'); });
});
