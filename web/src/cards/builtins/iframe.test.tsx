import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { act, fireEvent, render, screen, waitFor } from '@testing-library/react';
import type { KernelCard } from '../../api/wire';

const mocks = vi.hoisted(() => ({
  dlog: vi.fn(),
}));

vi.mock('../../api/calm', async () => {
  const actual =
    await vi.importActual<typeof import('../../api/calm')>('../../api/calm');
  return {
    ...actual,
    updateCard: vi.fn(),
  };
});

vi.mock('../../util/debug', () => ({
  dlog: mocks.dlog,
}));

import { IframeEntry } from './iframe';
import * as api from '../../api/calm';
import {
  __resetRegistryForTest,
  CardInstanceProvider,
  registerCard,
} from '../registry';
import {
  __resetCardEntryResolverRegistryForTest,
  resolveCardById,
} from '../resolver';
import type { IframeCardData } from './iframe';

function makeKernelCard(over: Partial<KernelCard> = {}): KernelCard {
  return {
    id: 'iframe_1',
    wave_id: 'wave_1',
    kind: 'iframe',
    sort: 0,
    payload: { url: 'https://example.com' },
    deletable: true,
    created_at: 1000,
    updated_at: 2000,
    ...over,
  };
}

function renderIframe(
  card: IframeCardData,
  opts: { onClose?: () => void } = {},
) {
  const Component = IframeEntry.Component;
  return render(
    <CardInstanceProvider cardId={card.id} deletable card={card}>
      <Component card={card} onClose={opts.onClose} />
    </CardInstanceProvider>,
  );
}

function iframeNode(): HTMLIFrameElement {
  return screen.getByTitle(/Embedded page:/) as HTMLIFrameElement;
}

describe('IframeEntry.fromKernel', () => {
  let warnSpy: ReturnType<typeof vi.spyOn>;

  beforeEach(() => {
    warnSpy = vi.spyOn(console, 'warn').mockImplementation(() => {});
  });

  afterEach(() => {
    warnSpy.mockRestore();
  });

  it('claims kind=iframe with a url payload', () => {
    const out = IframeEntry.fromKernel!(makeKernelCard());
    expect(out).toEqual({
      type: 'iframe',
      id: 'iframe_1',
      url: 'https://example.com',
    });
    expect(warnSpy).not.toHaveBeenCalled();
  });

  it('returns null for other kinds', () => {
    const out = IframeEntry.fromKernel!(
      makeKernelCard({ kind: 'file-viewer' }),
    );
    expect(out).toBeNull();
    expect(warnSpy).not.toHaveBeenCalled();
  });

  it('returns null and warns once for an invalid payload', () => {
    const invalid = makeKernelCard({ payload: {} });
    expect(IframeEntry.fromKernel!(invalid)).toBeNull();
    expect(IframeEntry.fromKernel!(invalid)).toBeNull();
    expect(warnSpy).toHaveBeenCalledTimes(1);
  });

  it('registers epoch refresh backing without onRefresh conflict', () => {
    __resetRegistryForTest();
    expect(() => registerCard(IframeEntry)).not.toThrow();
    expect(IframeEntry.refreshBacking).toBe('epoch');
  });
});

describe('IframeCard rendering', () => {
  let warnSpy: ReturnType<typeof vi.spyOn>;

  beforeEach(() => {
    __resetRegistryForTest();
    __resetCardEntryResolverRegistryForTest();
    registerCard(IframeEntry);
    mocks.dlog.mockClear();
    warnSpy = vi.spyOn(console, 'warn').mockImplementation(() => {});
    vi.mocked(api.updateCard).mockResolvedValue(makeKernelCard());
  });

  afterEach(() => {
    vi.clearAllMocks();
    __resetCardEntryResolverRegistryForTest();
    warnSpy.mockRestore();
  });

  it('renders the iframe with the initial URL', () => {
    renderIframe({ type: 'iframe', id: 'iframe_1', url: 'https://example.com' });

    const frame = screen.getByTitle('Embedded page: https://example.com');
    expect(frame).toHaveAttribute('src', 'https://example.com');
  });

  it('same-origin target stays opaque (no allow-same-origin)', () => {
    renderIframe({
      type: 'iframe',
      id: 'iframe_1',
      url: `${window.location.origin}/api/plugins/foo/resources/bar`,
    });

    const frame = screen.getByTitle(/Embedded page:/);
    const sandbox = frame.getAttribute('sandbox');
    expect(sandbox).toBeTruthy();
    expect(sandbox).toContain('allow-scripts');
    expect(sandbox).toContain('allow-popups');
    expect(sandbox).toContain('allow-forms');
    expect(sandbox).toContain('allow-popups-to-escape-sandbox');
    expect(sandbox).not.toContain('allow-same-origin');
  });

  it('cross-origin target gets allow-same-origin', () => {
    renderIframe({ type: 'iframe', id: 'iframe_1', url: 'https://example.com' });

    const frame = screen.getByTitle(/Embedded page:/);
    const sandbox = frame.getAttribute('sandbox');
    expect(sandbox).toBeTruthy();
    expect(sandbox).toContain('allow-scripts');
    expect(sandbox).toContain('allow-popups');
    expect(sandbox).toContain('allow-forms');
    expect(sandbox).toContain('allow-popups-to-escape-sandbox');
    expect(sandbox).toContain('allow-same-origin');
  });

  it('URL bar reflects current URL', () => {
    renderIframe({ type: 'iframe', id: 'iframe_1', url: 'https://example.com' });

    expect(screen.getByLabelText('Web page URL')).toHaveValue(
      'https://example.com',
    );
  });

  it('submitting a new URL updates the iframe src and persists it', async () => {
    renderIframe({ type: 'iframe', id: 'iframe_1', url: 'https://example.com' });

    const input = screen.getByLabelText('Web page URL');
    fireEvent.change(input, {
      target: { value: '  https://docs.mintlify.com/  ' },
    });
    fireEvent.click(screen.getByRole('button', { name: 'Go' }));

    const frame = screen.getByTitle(
      'Embedded page: https://docs.mintlify.com/',
    );
    expect(frame).toHaveAttribute('src', 'https://docs.mintlify.com/');
    expect(input).toHaveValue('https://docs.mintlify.com/');
    await waitFor(() => {
      expect(api.updateCard).toHaveBeenCalledWith('iframe_1', {
        payload: { url: 'https://docs.mintlify.com/' },
      });
    });
  });

  it('rejects javascript: URLs on submit', () => {
    renderIframe({ type: 'iframe', id: 'iframe_1', url: 'https://example.com' });

    const input = screen.getByLabelText('Web page URL');
    fireEvent.change(input, { target: { value: 'javascript:alert(1)' } });
    fireEvent.submit(input.closest('form')!);

    const frame = screen.getByTitle('Embedded page: https://example.com');
    expect(frame).toHaveAttribute('src', 'https://example.com');
    expect(input).toHaveValue('javascript:alert(1)');
    expect(api.updateCard).not.toHaveBeenCalled();
  });

  it('rejects data: URLs on submit', () => {
    renderIframe({ type: 'iframe', id: 'iframe_1', url: 'https://example.com' });

    const input = screen.getByLabelText('Web page URL');
    fireEvent.change(input, {
      target: { value: 'data:text/html,<script>alert(1)</script>' },
    });
    fireEvent.submit(input.closest('form')!);

    const frame = screen.getByTitle('Embedded page: https://example.com');
    expect(frame).toHaveAttribute('src', 'https://example.com');
    expect(input).toHaveValue('data:text/html,<script>alert(1)</script>');
    expect(api.updateCard).not.toHaveBeenCalled();
  });

  it('syncs local state when card.url changes externally', () => {
    const Component = IframeEntry.Component;
    const { rerender } = render(
      <CardInstanceProvider cardId="iframe_1" deletable>
        <Component card={{ type: 'iframe', id: 'iframe_1', url: 'https://a.com' }} />
      </CardInstanceProvider>,
    );

    rerender(
      <CardInstanceProvider cardId="iframe_1" deletable>
        <Component card={{ type: 'iframe', id: 'iframe_1', url: 'https://b.com' }} />
      </CardInstanceProvider>,
    );

    const frame = screen.getByTitle('Embedded page: https://b.com');
    expect(frame).toHaveAttribute('src', 'https://b.com');
    expect(screen.getByLabelText('Web page URL')).toHaveValue('https://b.com');
  });

  it('keeps optimistic URL when a stale card.url prop arrives mid-PATCH', () => {
    const Component = IframeEntry.Component;
    const { rerender } = render(
      <CardInstanceProvider cardId="iframe_1" deletable>
        <Component card={{ type: 'iframe', id: 'iframe_1', url: 'https://a.com' }} />
      </CardInstanceProvider>,
    );

    const input = screen.getByLabelText('Web page URL');
    fireEvent.change(input, { target: { value: 'https://b.com' } });
    fireEvent.submit(input.closest('form')!);

    expect(input).toHaveValue('https://b.com');
    expect(
      screen.getByTitle('Embedded page: https://b.com'),
    ).toHaveAttribute('src', 'https://b.com');

    rerender(
      <CardInstanceProvider cardId="iframe_1" deletable>
        <Component card={{ type: 'iframe', id: 'iframe_1', url: 'https://a.com' }} />
      </CardInstanceProvider>,
    );

    expect(screen.getByLabelText('Web page URL')).toHaveValue('https://b.com');
    expect(
      screen.getByTitle('Embedded page: https://b.com'),
    ).toHaveAttribute('src', 'https://b.com');
  });

  it('trims whitespace before persisting submitted URLs', async () => {
    renderIframe({ type: 'iframe', id: 'iframe_1', url: 'https://example.com' });

    const input = screen.getByLabelText('Web page URL');
    fireEvent.change(input, { target: { value: '  https://example.com  ' } });
    fireEvent.submit(input.closest('form')!);

    expect(input).toHaveValue('https://example.com');
    await waitFor(() => {
      expect(api.updateCard).toHaveBeenCalledWith('iframe_1', {
        payload: { url: 'https://example.com' },
      });
    });
  });

  it('submitting an empty URL does nothing', () => {
    renderIframe({ type: 'iframe', id: 'iframe_1', url: 'https://example.com' });

    const input = screen.getByLabelText('Web page URL');
    fireEvent.change(input, { target: { value: '   ' } });
    fireEvent.submit(input.closest('form')!);

    const frame = screen.getByTitle('Embedded page: https://example.com');
    expect(frame).toHaveAttribute('src', 'https://example.com');
    expect(input).toHaveValue('   ');
    expect(api.updateCard).not.toHaveBeenCalled();
  });

  it('calling onClose triggers the close handler', () => {
    const onClose = vi.fn();
    renderIframe(
      { type: 'iframe', id: 'iframe_1', url: 'https://example.com' },
      { onClose },
    );

    fireEvent.click(screen.getByRole('button', { name: 'Remove panel' }));
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it('reload action remounts the iframe element', async () => {
    renderIframe({ type: 'iframe', id: 'iframe_1', url: 'https://example.com' });

    const before = iframeNode();
    fireEvent.click(screen.getByRole('button', { name: 'Reload' }));

    let after = iframeNode();
    await waitFor(() => {
      after = iframeNode();
      expect(after).not.toBe(before);
    });
    expect(after).toHaveAttribute('src', 'https://example.com');
  });

  it('logs visibility hints without returning onRefresh', async () => {
    renderIframe({ type: 'iframe', id: 'iframe_1', url: 'https://example.com' });

    await waitFor(() =>
      expect(resolveCardById('iframe_1')?.writer).toBeDefined(),
    );
    act(() => {
      resolveCardById('iframe_1')!.writer.setVisible(false);
    });

    expect(mocks.dlog).toHaveBeenCalledWith('IframeCard', 'visibility', {
      cardId: 'iframe_1',
      visible: false,
    });
  });
});
