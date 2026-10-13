// @vitest-environment jsdom
import { cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, expect, it, vi } from 'vitest';
import { ReportFileViewer } from './public.tsx';

afterEach(cleanup);

const PAGE = '<!doctype html><title>Overview</title><svg></svg>';

it('renders an HTML file in the sandboxed page frame, with its source one toggle away', async () => {
  const readFile = vi.fn(() => Promise.resolve({ path: 'docs/overview.html', size: PAGE.length, text: PAGE, truncated: false }));
  const onFileOpened = vi.fn();
  const { container } = render(<ReportFileViewer path="docs/overview.html"
    files={{ readFile, rawUrl: (path) => `/raw?path=${path}` }} fileRoot="/repo" wide onClose={() => {}}
    onFileOpened={onFileOpened} />);
  await screen.findByRole('button', { name: 'Page' });
  const frame = container.querySelector('iframe');
  if (frame === null) throw new Error('the page frame is mounted');
  expect(frame.title).toBe('docs/overview.html');
  expect(frame.getAttribute('sandbox')).toBe('allow-scripts');
  expect(frame.getAttribute('src')).toBe('/raw?path=docs/overview.html');
  expect(screen.getByRole('button', { name: 'Page' }).getAttribute('aria-pressed')).toBe('true');
  expect(onFileOpened).toHaveBeenCalledWith('docs/overview.html');

  await userEvent.click(screen.getByRole('button', { name: 'Source' }));
  expect(container.querySelector('iframe')).toBeNull();
  expect(container.querySelector('[data-nc-report-file-source]')?.textContent).toContain('<title>Overview</title>');

  await userEvent.click(screen.getByRole('button', { name: 'Page' }));
  expect(container.querySelector('iframe')?.getAttribute('sandbox')).toBe('allow-scripts');
});
