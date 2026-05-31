import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { fireEvent, render, screen, waitFor } from '@testing-library/react';
import type { KernelCard } from '../../api/wire';

vi.mock('../../api/calm', async () => {
  const actual =
    await vi.importActual<typeof import('../../api/calm')>('../../api/calm');
  return {
    ...actual,
    updateCard: vi.fn(),
  };
});

import { IframeEntry } from './iframe';
import * as api from '../../api/calm';

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
});

describe('IframeCard rendering', () => {
  beforeEach(() => {
    vi.mocked(api.updateCard).mockResolvedValue(makeKernelCard());
  });

  afterEach(() => {
    vi.clearAllMocks();
  });

  it('renders the iframe with the initial URL', () => {
    const Component = IframeEntry.Component;
    render(
      <Component
        card={{ type: 'iframe', id: 'iframe_1', url: 'https://example.com' }}
      />,
    );

    const frame = screen.getByTitle('Embedded page: https://example.com');
    expect(frame).toHaveAttribute('src', 'https://example.com');
  });

  it('URL bar reflects current URL', () => {
    const Component = IframeEntry.Component;
    render(
      <Component
        card={{ type: 'iframe', id: 'iframe_1', url: 'https://example.com' }}
      />,
    );

    expect(screen.getByLabelText('Web page URL')).toHaveValue(
      'https://example.com',
    );
  });

  it('submitting a new URL updates the iframe src and persists it', async () => {
    const Component = IframeEntry.Component;
    render(
      <Component
        card={{ type: 'iframe', id: 'iframe_1', url: 'https://example.com' }}
      />,
    );

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

  it('submitting an empty URL does nothing', () => {
    const Component = IframeEntry.Component;
    render(
      <Component
        card={{ type: 'iframe', id: 'iframe_1', url: 'https://example.com' }}
      />,
    );

    const input = screen.getByLabelText('Web page URL');
    fireEvent.change(input, { target: { value: '   ' } });
    fireEvent.submit(input.closest('form')!);

    const frame = screen.getByTitle('Embedded page: https://example.com');
    expect(frame).toHaveAttribute('src', 'https://example.com');
    expect(input).toHaveValue('   ');
    expect(api.updateCard).not.toHaveBeenCalled();
  });

  it('calling onClose triggers the close handler', () => {
    const Component = IframeEntry.Component;
    const onClose = vi.fn();
    render(
      <Component
        card={{ type: 'iframe', id: 'iframe_1', url: 'https://example.com' }}
        onClose={onClose}
      />,
    );

    fireEvent.click(screen.getByRole('button', { name: 'Remove panel' }));
    expect(onClose).toHaveBeenCalledTimes(1);
  });
});
