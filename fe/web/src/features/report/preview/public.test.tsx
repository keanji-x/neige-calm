// @vitest-environment jsdom
// @vitest-environment-options {"url":"http://192.168.5.20:4040/next/tracks/w1"}
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';

import type { PreviewResolution } from '../../../../../core/domain/report.ts';
import { previewFrameUrl, ReportPreviewBlock } from './public.tsx';

afterEach(cleanup);

function registered(live: boolean, port = 4050): PreviewResolution {
  return { status: 'registered', preview: { key: 'fe', title: 'Dev FE', port, live } };
}

describe('previewFrameUrl', () => {
  it('keeps the page scheme and host and swaps in the gateway port and the path', () => {
    expect(previewFrameUrl({ protocol: 'http:', hostname: '192.168.5.20' }, 4050, '/next/'))
      .toBe('http://192.168.5.20:4050/next/');
    expect(previewFrameUrl({ protocol: 'http:', hostname: 'devbox.local' }, 4051, undefined)).toBe('http://devbox.local:4051/');
    expect(previewFrameUrl({ protocol: 'http:', hostname: '[::1]' }, 4052, null)).toBe('http://[::1]:4052/');
  });
});

describe('ReportPreviewBlock', () => {
  it('frames the gateway port on this page\'s host, at the payload path', () => {
    const { container } = render(<ReportPreviewBlock payload={{ key: 'fe', path: '/next/' }}
      resolve={() => registered(true)} />);
    const frame = container.querySelector('iframe');
    expect(frame?.getAttribute('src')).toBe('http://192.168.5.20:4050/next/');
    // The registration's title names the frame when the payload has none.
    expect(frame?.getAttribute('title')).toBe('Dev FE');
  });

  it('asks the resolver by the block key', () => {
    const resolve = vi.fn(() => registered(true));
    render(<ReportPreviewBlock payload={{ key: 'fe', title: 'Mine' }} resolve={resolve} />);
    expect(resolve).toHaveBeenCalledWith('fe');
    expect(screen.getByTitle('Mine').getAttribute('src')).toBe('http://192.168.5.20:4050/');
  });

  it('sandboxes the frame with exactly the preview grant and sends no referrer', () => {
    const { container } = render(<ReportPreviewBlock payload={{ key: 'fe' }} resolve={() => registered(true)} />);
    const frame = container.querySelector('iframe');
    expect(frame?.getAttribute('sandbox')).toBe('allow-scripts allow-same-origin allow-forms allow-popups allow-modals');
    expect(frame?.getAttribute('allow')).toBe('fullscreen');
    expect(frame?.getAttribute('referrerpolicy')).toBe('no-referrer');
  });

  it('takes its height from the payload, clamped, and defaults like the app block', () => {
    const { container, rerender } = render(<ReportPreviewBlock payload={{ key: 'fe', height: 720 }}
      resolve={() => registered(true)} />);
    const stage = () => container.querySelector('iframe')?.parentElement;
    expect(stage()?.style.blockSize).toBe('720px');
    rerender(<ReportPreviewBlock payload={{ key: 'fe' }} resolve={() => registered(true)} />);
    expect(stage()?.style.blockSize).toBe('360px');
  });

  it('toggles the frame between desktop width and a 390 px phone', () => {
    const { container } = render(<ReportPreviewBlock payload={{ key: 'fe' }} resolve={() => registered(true)} />);
    const frame = () => container.querySelector('iframe');
    const desktop = screen.getByRole('button', { name: 'Desktop' });
    const mobile = screen.getByRole('button', { name: 'Mobile' });
    expect(desktop.getAttribute('aria-pressed')).toBe('true');
    expect(frame()?.style.inlineSize).toBe('');
    fireEvent.click(mobile);
    expect(mobile.getAttribute('aria-pressed')).toBe('true');
    expect(desktop.getAttribute('aria-pressed')).toBe('false');
    expect(frame()?.style.inlineSize).toBe('390px');
    fireEvent.click(desktop);
    expect(frame()?.style.inlineSize).toBe('');
  });

  it('asks the figure, not the frame, for fullscreen', () => {
    const { container } = render(<ReportPreviewBlock payload={{ key: 'fe' }} resolve={() => registered(true)} />);
    const figure = container.querySelector('figure');
    const requestFullscreen = vi.fn(() => Promise.resolve());
    if (figure !== null) figure.requestFullscreen = requestFullscreen;
    fireEvent.click(screen.getByRole('button', { name: 'Fullscreen' }));
    expect(requestFullscreen).toHaveBeenCalledTimes(1);
  });

  it('names the key when nothing is registered under it, and draws no frame', () => {
    const { container } = render(<ReportPreviewBlock payload={{ key: 'fe' }} resolve={() => ({ status: 'missing' })} />);
    expect(container.querySelector('iframe')).toBeNull();
    expect(screen.getByRole('note').textContent).toBe('No preview is registered under “fe”.');
  });

  it('says the dev server is offline while the registration does not answer', () => {
    const { container } = render(<ReportPreviewBlock payload={{ key: 'fe' }} resolve={() => registered(false)} />);
    expect(container.querySelector('iframe')).toBeNull();
    expect(screen.getByRole('note').textContent)
      .toBe('Preview “fe” is offline — waiting for its dev server on port 4050.');
  });

  it('mounts a fresh frame when the dev server comes back', () => {
    const { container, rerender } = render(<ReportPreviewBlock payload={{ key: 'fe' }}
      resolve={() => registered(true)} />);
    const before = container.querySelector('iframe');
    expect(before).not.toBeNull();
    rerender(<ReportPreviewBlock payload={{ key: 'fe' }} resolve={() => registered(false)} />);
    expect(container.querySelector('iframe')).toBeNull();
    rerender(<ReportPreviewBlock payload={{ key: 'fe' }} resolve={() => registered(true)} />);
    const after = container.querySelector('iframe');
    expect(after).not.toBeNull();
    expect(after).not.toBe(before);
    expect(before?.isConnected).toBe(false);
  });

  it('says so when this view carries no previews, or the read is loading or failed', () => {
    const { rerender } = render(<ReportPreviewBlock payload={{ key: 'fe' }} />);
    expect(screen.getByRole('note').textContent).toBe('This view does not carry previews.');
    rerender(<ReportPreviewBlock payload={{ key: 'fe' }} resolve={() => ({ status: 'loading' })} />);
    expect(screen.getByRole('note').textContent).toBe('Loading …');
    rerender(<ReportPreviewBlock payload={{ key: 'fe' }} resolve={() => ({ status: 'error', message: 'boom' })} />);
    expect(screen.getByRole('note').textContent).toBe('Could not read previews: boom');
  });
});
