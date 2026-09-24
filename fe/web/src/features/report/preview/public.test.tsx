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

  it('takes its height from the payload, clamped, and defaults like the app block', () => {
    const { container, rerender } = render(<ReportPreviewBlock payload={{ key: 'fe', height: 720 }}
      resolve={() => registered(true)} />);
    const stage = () => container.querySelector('iframe')?.parentElement;
    expect(stage()?.style.blockSize).toBe('720px');
    rerender(<ReportPreviewBlock payload={{ key: 'fe' }} resolve={() => registered(true)} />);
    expect(stage()?.style.blockSize).toBe('360px');
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

function device(value: string) {
  fireEvent.change(screen.getByRole('combobox', { name: 'Device' }), { target: { value } });
}

describe('ReportPreviewBlock device frame', () => {
  const frame = () => document.querySelector('iframe');

  it('opens on Fit: the column\'s width, the payload\'s height, no transform, rotate disabled', () => {
    stubStage(700);
    render(<ReportPreviewBlock payload={{ key: 'fe', height: 500 }} resolve={() => registered(true)} />);
    expect(screen.getByRole<HTMLSelectElement>('combobox', { name: 'Device' }).value).toBe('fit');
    expect(frame()?.style.transform).toBe('');
    expect(frame()?.style.inlineSize).toBe('');
    expect(frame()?.parentElement?.style.blockSize).toBe('500px');
    expect(screen.getByRole<HTMLButtonElement>('button', { name: 'Rotate' }).disabled).toBe(true);
    expect(screen.getByText('700 × 500 · 100%')).toBeTruthy();
  });

  it('gives a device preset its true CSS-pixel box and scales it from the stage width', () => {
    stubStage(600);
    render(<ReportPreviewBlock payload={{ key: 'fe', height: 2000 }} resolve={() => registered(true)} />);
    device('ipad');
    expect(frame()?.style.inlineSize).toBe('820px');
    expect(frame()?.style.blockSize).toBe('1180px');
    // 600 / 820 — the width binds, the 2000 px stage height does not.
    const scale = 600 / 820;
    expect(frame()?.style.transform).toBe(`scale(${scale})`);
    // The wrapper takes the scaled box: no dead space under the shrunk device.
    expect(frame()?.parentElement?.style.inlineSize).toBe(`${820 * scale}px`);
    expect(frame()?.parentElement?.style.blockSize).toBe(`${1180 * scale}px`);
    expect(screen.getByText('820 × 1180 · 73%')).toBeTruthy();
  });

  it('lets the payload height bind when it is tighter than the width, and never enlarges', () => {
    stubStage(2000);
    render(<ReportPreviewBlock payload={{ key: 'fe', height: 426 }} resolve={() => registered(true)} />);
    device('iphone-15');
    expect(frame()?.style.transform).toBe('scale(0.5)');
    device('fit');
    device('laptop');
    expect(frame()?.style.transform).toBe(`scale(${426 / 800})`);
  });

  it('rescales when the column resizes', () => {
    const stage = stubStage(1440);
    render(<ReportPreviewBlock payload={{ key: 'fe', height: 2000 }} resolve={() => registered(true)} />);
    device('desktop');
    expect(frame()?.style.transform).toBe('scale(1)');
    stage.resize(720);
    expect(frame()?.style.transform).toBe('scale(0.5)');
  });

  it('rotate swaps width and height', () => {
    stubStage(5000);
    render(<ReportPreviewBlock payload={{ key: 'fe', height: 5000 }} resolve={() => registered(true)} />);
    device('pixel-8');
    fireEvent.click(screen.getByRole('button', { name: 'Rotate' }));
    expect(frame()?.style.inlineSize).toBe('915px');
    expect(frame()?.style.blockSize).toBe('412px');
    fireEvent.click(screen.getByRole('button', { name: 'Rotate' }));
    expect(frame()?.style.inlineSize).toBe('412px');
  });

  it('clamps a custom size to 240..3840 on commit, and rotates it by swapping its numbers', () => {
    stubStage(5000);
    render(<ReportPreviewBlock payload={{ key: 'fe', height: 5000 }} resolve={() => registered(true)} />);
    device('custom');
    const width = () => screen.getByRole<HTMLInputElement>('spinbutton', { name: 'Width' });
    const height = () => screen.getByRole<HTMLInputElement>('spinbutton', { name: 'Height' });
    fireEvent.change(width(), { target: { value: '100' } });
    fireEvent.change(height(), { target: { value: '9000' } });
    fireEvent.blur(height());
    expect(frame()?.style.inlineSize).toBe('240px');
    expect(frame()?.style.blockSize).toBe('3840px');
    expect(width().value).toBe('240');
    fireEvent.change(width(), { target: { value: '1000' } });
    fireEvent.keyDown(width(), { key: 'Enter' });
    expect(frame()?.style.inlineSize).toBe('1000px');
    fireEvent.click(screen.getByRole('button', { name: 'Rotate' }));
    expect(frame()?.style.inlineSize).toBe('3840px');
    expect(frame()?.style.blockSize).toBe('1000px');
    expect(width().value).toBe('3840');
  });

  it('remembers the choice per key through the store it is given', () => {
    stubStage(5000);
    const viewports = memoryViewports();
    const view = render(<ReportPreviewBlock payload={{ key: 'fe', height: 5000 }} viewports={viewports}
      resolve={() => registered(true)} />);
    device('pixel-8');
    fireEvent.click(screen.getByRole('button', { name: 'Rotate' }));
    view.unmount();
    render(<ReportPreviewBlock payload={{ key: 'fe', height: 5000 }} viewports={viewports}
      resolve={() => registered(true)} />);
    expect(screen.getByRole<HTMLSelectElement>('combobox', { name: 'Device' }).value).toBe('pixel-8');
    expect(frame()?.style.inlineSize).toBe('915px');
    cleanup();
    // Another key in the same report: its own choice.
    render(<ReportPreviewBlock payload={{ key: 'api', height: 5000 }} viewports={viewports}
      resolve={() => registered(true)} />);
    expect(screen.getByRole<HTMLSelectElement>('combobox', { name: 'Device' }).value).toBe('fit');
    expect([...viewports.values.keys()]).toEqual(['fe']);
  });

  it('reads a stored custom size back clamped, and ignores a foreign shape', () => {
    stubStage(5000);
    const viewports = memoryViewports();
    viewports.write('fe', JSON.stringify({ preset: 'custom', custom: { width: 10, height: 99999 }, rotated: false }));
    viewports.write('api', '{"preset":"watch"}');
    render(<ReportPreviewBlock payload={{ key: 'fe', height: 5000 }} viewports={viewports}
      resolve={() => registered(true)} />);
    expect(frame()?.style.inlineSize).toBe('240px');
    expect(frame()?.style.blockSize).toBe('3840px');
    cleanup();
    render(<ReportPreviewBlock payload={{ key: 'api', height: 5000 }} viewports={viewports}
      resolve={() => registered(true)} />);
    expect(screen.getByRole<HTMLSelectElement>('combobox', { name: 'Device' }).value).toBe('fit');
  });

  it('still renders, and still switches devices, when the store throws', () => {
    stubStage(5000);
    const viewports = {
      read: () => { throw new Error('denied'); },
      write: () => { throw new Error('denied'); },
    };
    render(<ReportPreviewBlock payload={{ key: 'fe', height: 5000 }} viewports={viewports}
      resolve={() => registered(true)} />);
    expect(frame()).not.toBeNull();
    device('iphone-15');
    expect(frame()?.style.inlineSize).toBe('393px');
  });

  it('keeps the notices inside the frame, under the title bar', () => {
    render(<ReportPreviewBlock payload={{ key: 'fe', title: 'Mine' }} resolve={() => registered(false)} />);
    const figure = document.querySelector('figure');
    expect(figure?.querySelector('figcaption')?.textContent).toBe('Mine');
    expect(figure?.contains(screen.getByRole('note'))).toBe(true);
  });
});
