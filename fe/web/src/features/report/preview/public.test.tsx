// @vitest-environment jsdom
// @vitest-environment-options {"url":"http://192.168.5.20:4040/next/tracks/w1"}
import { act, cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';

import type { PreviewResolution } from '../../../../../core/domain/report.ts';
import { previewFrameUrl, ReportPreviewBlock } from './public.tsx';

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

/** The app's display preferences, as the block sees them: a string per block key. */
function memoryViewports() {
  const values = new Map<string, string>();
  return { values, read: (key: string) => values.get(key) ?? null, write: (key: string, value: string) => { values.set(key, value); } };
}

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

  it('takes its stage height from the payload, clamped, and defaults like the app block', () => {
    // Unmeasured width: only the payload height binds the Desktop device (1080 px tall).
    const { container, rerender } = render(<ReportPreviewBlock payload={{ key: 'fe', height: 540 }}
      resolve={() => registered(true)} />);
    const frame = () => container.querySelector('iframe');
    expect(frame()?.style.transform).toBe('scale(0.5)');
    rerender(<ReportPreviewBlock payload={{ key: 'fe', height: 5 }} resolve={() => registered(true)} />);
    expect(frame()?.style.transform).toBe(`scale(${120 / 1080})`);
    rerender(<ReportPreviewBlock payload={{ key: 'fe' }} resolve={() => registered(true)} />);
    expect(frame()?.style.transform).toBe(`scale(${360 / 1080})`);
  });

  it('asks the figure, not the frame, for fullscreen', () => {
    const { container } = render(<ReportPreviewBlock payload={{ key: 'fe' }} resolve={() => registered(true)} />);
    const figure = container.querySelector('figure');
    const requestFullscreen = vi.fn(() => Promise.resolve());
    if (figure !== null) figure.requestFullscreen = requestFullscreen;
    fireEvent.click(screen.getByRole('button', { name: 'Fullscreen' }));
    expect(requestFullscreen).toHaveBeenCalledTimes(1);
    // A stroked icon in currentColor, not a font glyph that may render as tofu.
    const button = screen.getByRole('button', { name: 'Fullscreen' });
    expect(button.textContent).toBe('');
    expect(button.querySelector('svg')?.getAttribute('stroke')).toBe('currentColor');
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

/** jsdom lays nothing out: the stage's width is whatever this says, and resizes are delivered by hand. */
function stubStage(width: number, height = 0) {
  const size = { width, height };
  /* Only an observer that is observing hears a resize. */
  const observing = new Set<ResizeObserverCallback>();
  vi.spyOn(HTMLElement.prototype, 'clientWidth', 'get').mockImplementation(() => size.width);
  vi.spyOn(HTMLElement.prototype, 'clientHeight', 'get').mockImplementation(() => size.height);
  vi.stubGlobal('ResizeObserver', class {
    readonly callback: ResizeObserverCallback;
    constructor(callback: ResizeObserverCallback) { this.callback = callback; }
    observe() { observing.add(this.callback); }
    disconnect() { observing.delete(this.callback); }
  });
  return {
    resize(nextWidth: number) {
      size.width = nextWidth;
      act(() => { for (const callback of observing) callback([], {} as ResizeObserver); });
    },
  };
}

function toggle(name: 'Desktop' | 'Mobile') {
  fireEvent.click(screen.getByRole('button', { name }));
}


describe('ReportPreviewBlock device frame', () => {
  const frame = () => document.querySelector('iframe');

  it('opens on Desktop at its true 1920 × 1080, scaled from the stage width', () => {
    stubStage(960);
    render(<ReportPreviewBlock payload={{ key: 'fe', height: 2000 }} resolve={() => registered(true)} />);
    expect(screen.getByRole('button', { name: 'Desktop' }).getAttribute('aria-pressed')).toBe('true');
    expect(frame()?.style.inlineSize).toBe('1920px');
    expect(frame()?.style.blockSize).toBe('1080px');
    // 960 / 1920 — the width binds, the 2000 px stage height does not.
    expect(frame()?.style.transform).toBe('scale(0.5)');
    // The wrapper takes the scaled box: no dead space under the shrunk device.
    expect(frame()?.parentElement?.style.inlineSize).toBe('960px');
    expect(frame()?.parentElement?.style.blockSize).toBe('540px');
    expect(screen.getByText('1920 × 1080 · 50%')).toBeTruthy();
  });

  it('gives Mobile its true 390 × 844 and never enlarges it', () => {
    stubStage(2000);
    render(<ReportPreviewBlock payload={{ key: 'fe', height: 2000 }} resolve={() => registered(true)} />);
    toggle('Mobile');
    expect(screen.getByRole('button', { name: 'Mobile' }).getAttribute('aria-pressed')).toBe('true');
    expect(screen.getByRole('button', { name: 'Desktop' }).getAttribute('aria-pressed')).toBe('false');
    expect(frame()?.style.inlineSize).toBe('390px');
    expect(frame()?.style.blockSize).toBe('844px');
    expect(frame()?.style.transform).toBe('scale(1)');
    expect(screen.getByText('390 × 844 · 100%')).toBeTruthy();
    toggle('Desktop');
    expect(frame()?.style.inlineSize).toBe('1920px');
  });

  it('rescales when the column resizes', () => {
    const stage = stubStage(1920);
    render(<ReportPreviewBlock payload={{ key: 'fe', height: 2000 }} resolve={() => registered(true)} />);
    expect(frame()?.style.transform).toBe('scale(1)');
    stage.resize(480);
    expect(frame()?.style.transform).toBe('scale(0.25)');
  });

  it('remembers the toggle per key through the store it is given', () => {
    stubStage(2000);
    const viewports = memoryViewports();
    const view = render(<ReportPreviewBlock payload={{ key: 'fe', height: 2000 }} viewports={viewports}
      resolve={() => registered(true)} />);
    toggle('Mobile');
    view.unmount();
    render(<ReportPreviewBlock payload={{ key: 'fe', height: 2000 }} viewports={viewports}
      resolve={() => registered(true)} />);
    expect(screen.getByRole('button', { name: 'Mobile' }).getAttribute('aria-pressed')).toBe('true');
    expect(frame()?.style.inlineSize).toBe('390px');
    cleanup();
    // Another key in the same report: its own choice.
    render(<ReportPreviewBlock payload={{ key: 'api', height: 2000 }} viewports={viewports}
      resolve={() => registered(true)} />);
    expect(frame()?.style.inlineSize).toBe('1920px');
    expect([...viewports.values.entries()]).toEqual([['fe', 'mobile']]);
  });

  it('reads the new key\'s choice when the same block is rewritten to another preview key', () => {
    stubStage(2000);
    const viewports = memoryViewports();
    viewports.write('fe', 'mobile');
    viewports.write('api', 'desktop');
    const { rerender } = render(<ReportPreviewBlock payload={{ key: 'fe', height: 2000 }} viewports={viewports}
      resolve={() => registered(true)} />);
    expect(frame()?.style.inlineSize).toBe('390px');
    rerender(<ReportPreviewBlock payload={{ key: 'api', height: 2000 }} viewports={viewports}
      resolve={() => registered(true)} />);
    expect(screen.getByRole('button', { name: 'Desktop' }).getAttribute('aria-pressed')).toBe('true');
    expect(frame()?.style.inlineSize).toBe('1920px');
    // Another track's store, same key: that track's choice.
    const other = memoryViewports();
    other.write('api', 'mobile');
    rerender(<ReportPreviewBlock payload={{ key: 'api', height: 2000 }} viewports={other}
      resolve={() => registered(true)} />);
    expect(frame()?.style.inlineSize).toBe('390px');
    // A toggle after the switch is remembered under the new pair only.
    toggle('Desktop');
    expect(other.values.get('api')).toBe('desktop');
    expect(viewports.values.get('fe')).toBe('mobile');
  });

  it.each([
    'iphone-15', 'custom', 'fit', '',
    JSON.stringify({ preset: 'custom', custom: { width: 800, height: 600 }, rotated: true }),
    JSON.stringify({ preset: 'mobile' }),
  ])('opens an unknown or older stored value %j on Desktop', (stored) => {
    stubStage(2000);
    const viewports = memoryViewports();
    viewports.write('fe', stored);
    render(<ReportPreviewBlock payload={{ key: 'fe', height: 2000 }} viewports={viewports}
      resolve={() => registered(true)} />);
    expect(screen.getByRole('button', { name: 'Desktop' }).getAttribute('aria-pressed')).toBe('true');
    expect(frame()?.style.inlineSize).toBe('1920px');
  });

  it('still renders, and still toggles, when the store throws', () => {
    stubStage(2000);
    const viewports = {
      read: () => { throw new Error('denied'); },
      write: () => { throw new Error('denied'); },
    };
    render(<ReportPreviewBlock payload={{ key: 'fe', height: 2000 }} viewports={viewports}
      resolve={() => registered(true)} />);
    expect(frame()?.style.inlineSize).toBe('1920px');
    toggle('Mobile');
    expect(frame()?.style.inlineSize).toBe('390px');
  });

  it('keeps the notices inside the frame, under the title bar', () => {
    render(<ReportPreviewBlock payload={{ key: 'fe', title: 'Mine' }} resolve={() => registered(false)} />);
    const figure = document.querySelector('figure');
    expect(figure?.querySelector('figcaption')?.textContent).toBe('Mine');
    expect(figure?.contains(screen.getByRole('note'))).toBe(true);
  });
});
