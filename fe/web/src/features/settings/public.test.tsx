// @vitest-environment jsdom
import { act, cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { HTTPS_PROXY_KEY, HTTP_PROXY_KEY } from '../../../../core/domain/settings.ts';
import { AppearancePane, NetworkPane, type NetworkPaneProps } from './public.tsx';

beforeEach(() => {
  vi.stubGlobal('matchMedia', vi.fn(() => ({
    matches: false,
    addEventListener: vi.fn(),
    removeEventListener: vi.fn(),
  })));
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

function props(overrides: Partial<NetworkPaneProps> = {}): NetworkPaneProps {
  return {
    settings: {},
    loadError: null,
    saving: false,
    saveError: null,
    savedAt: null,
    onSave: vi.fn(),
    onRetryLoad: vi.fn(),
    ...overrides,
  };
}

describe('Settings network form', () => {
  it('seeds the proxy fields from the settings bag', () => {
    render(<NetworkPane {...props({
      settings: { [HTTP_PROXY_KEY]: 'http://box:3128', [HTTPS_PROXY_KEY]: 'http://box:3129' },
    })} />);
    expect(screen.getByLabelText<HTMLInputElement>('HTTP proxy').value).toBe('http://box:3128');
    expect(screen.getByLabelText<HTMLInputElement>('HTTPS proxy').value).toBe('http://box:3129');
  });

  it('shows an empty field when the key is absent from the bag', () => {
    render(<NetworkPane {...props()} />);
    expect(screen.getByLabelText<HTMLInputElement>('HTTP proxy').value).toBe('');
  });

  it('sends the edited value under the domain key', async () => {
    const onSave = vi.fn();
    render(<NetworkPane {...props({ onSave })} />);
    await userEvent.type(screen.getByLabelText('HTTPS proxy'), 'http://edge:8080');
    await userEvent.click(screen.getByRole('button', { name: 'Save' }));
    expect(onSave).toHaveBeenCalledWith({ [HTTPS_PROXY_KEY]: 'http://edge:8080' });
  });

  // CR-6 — in flight the button is *busy*, not `disabled`. Astryx announces
  // that with aria-busy and its spinner; the handler remains the activation
  // guard, so focus is not thrown to <body> mid-action.
  it('flips the save label and blocks the button while saving, without disabling it', () => {
    render(<NetworkPane {...props({ saving: true })} />);
    const save = screen.getByRole('button', { name: 'Saving…' });
    expect(save.hasAttribute('disabled')).toBe(false);
    expect(save.getAttribute('aria-disabled')).toBeNull();
    expect(save.getAttribute('aria-busy')).toBe('true');
    expect(save.dataset.ncState).toBe('busy');
  });

  it('marks only the ready Save label as visible before saving', async () => {
    render(<NetworkPane {...props()} />);
    await userEvent.type(screen.getByLabelText('HTTP proxy'), 'http://edge');
    const save = screen.getByRole('button', { name: 'Save' });
    expect(save.textContent).toContain('Save');
    expect(screen.queryByRole('button', { name: 'Saving…' })).toBeNull();
    expect(save.dataset.ncState).toBeUndefined();
  });

  it('re-seeds the fields when the settings prop reports a new server value', () => {
    const view = render(<NetworkPane {...props({ settings: { [HTTP_PROXY_KEY]: 'http://old' } })} />);
    view.rerender(<NetworkPane {...props({ settings: { [HTTP_PROXY_KEY]: 'http://new' } })} />);
    expect(screen.getByLabelText<HTMLInputElement>('HTTP proxy').value).toBe('http://new');
  });

  it('keeps what the user typed when the parent re-renders with an equal bag', async () => {
    const view = render(<NetworkPane {...props({ settings: { [HTTP_PROXY_KEY]: 'http://box' } })} />);
    await userEvent.clear(screen.getByLabelText('HTTP proxy'));
    await userEvent.type(screen.getByLabelText('HTTP proxy'), 'http://typed');
    // A fresh object with identical values — a query cache does this.
    view.rerender(<NetworkPane {...props({ settings: { [HTTP_PROXY_KEY]: 'http://box' } })} />);
    expect(screen.getByLabelText<HTMLInputElement>('HTTP proxy').value).toBe('http://typed');
  });

  it('preserves a field edited during an in-flight save when the response updates another field', async () => {
    const view = render(<NetworkPane {...props({
      settings: { [HTTP_PROXY_KEY]: 'http://old-http', [HTTPS_PROXY_KEY]: 'http://old-https' },
      saving: true,
    })} />);
    await userEvent.clear(screen.getByLabelText('HTTPS proxy'));
    await userEvent.type(screen.getByLabelText('HTTPS proxy'), 'http://typed-during-save');
    view.rerender(<NetworkPane {...props({
      settings: { [HTTP_PROXY_KEY]: 'http://saved-http', [HTTPS_PROXY_KEY]: 'http://old-https' },
      saving: false, savedAt: 123,
    })} />);
    expect(screen.getByLabelText<HTMLInputElement>('HTTPS proxy').value).toBe('http://typed-during-save');
  });
});

describe('Settings appearance', () => {
  it('states the current theme on a dropdown and reports the one the reader picks', async () => {
    const onThemeModeChange = vi.fn();
    render(<AppearancePane themeMode="system" onThemeModeChange={onThemeModeChange} />);
    const control = screen.getByRole('combobox', { name: 'Theme' });
    expect(control.textContent).toContain('System');

    await userEvent.click(control);
    await userEvent.click(await screen.findByRole('option', { name: 'Dark' }));
    expect(onThemeModeChange).toHaveBeenCalledWith('dark');
  });

  it('shows the active mode, not the first option', () => {
    render(<AppearancePane themeMode="light" onThemeModeChange={vi.fn()} />);
    expect(screen.getByRole('combobox', { name: 'Theme' }).textContent).toContain('Light');
  });
});

describe('Settings states', () => {
  it('surfaces a load failure as an alert', () => {
    render(<NetworkPane {...props({ settings: undefined, loadError: 'settings unreachable' })} />);
    expect(screen.getByRole('alert')).toBeTruthy();
    expect(screen.getByText('settings unreachable')).toBeTruthy();
    expect(screen.queryByText('Loading settings…')).toBeNull();
  });

  it('retries a failed settings read from the in-place error', async () => {
    const onRetryLoad = vi.fn();
    render(<NetworkPane {...props({ settings: undefined, loadError: 'settings unreachable', onRetryLoad })} />);
    await userEvent.click(screen.getByRole('button', { name: 'Retry' }));
    expect(onRetryLoad).toHaveBeenCalledTimes(1);
  });

  it('surfaces a save failure as an alert', () => {
    render(<NetworkPane {...props({ saveError: 'PUT /api/settings failed' })} />);
    expect(screen.getByRole('alert').textContent).toBe('PUT /api/settings failed');
  });

  it('confirms a save through a status role and then retires the notice', () => {
    vi.useFakeTimers();
    try {
      render(<NetworkPane {...props({ savedAt: 1234, savedNoticeMs: 10 })} />);
      expect(screen.getByText('Saved.').getAttribute('role')).toBe('status');
      act(() => { vi.advanceTimersByTime(20); });
      expect(screen.queryByText('Saved.')).toBeNull();
    } finally {
      vi.useRealTimers();
    }
  });

  /*
   * The e2e locator, pinned where it can actually run (`fe e2e` needs the real
   * stack). `settings-roundtrip.spec.ts` reads `[data-nc-settings-saved]` and
   * then asserts its text, because `role="status"` resolves to three elements
   * on this page — each Astryx `Button` renders an unconditional empty live
   * region with that role. Locating by the anchor and asserting the text are
   * two independent claims; filtering the role by 'Saved.' would collapse them
   * into one and could never distinguish "the save succeeded" from "the string
   * happens to be on the page".
   *
   * Both halves are load-bearing: the anchor must appear *only* after a save,
   * and it must carry that text. Deleting the attribute, or dropping the
   * `showSaved &&` guard so the notice is always mounted, must turn this red.
   */
  it('exposes the saved notice at the anchor the e2e spec locates, and only after a save', () => {
    const { unmount } = render(<NetworkPane {...props({ savedAt: null })} />);
    expect(document.querySelectorAll('[data-nc-settings-saved]')).toHaveLength(0);
    unmount();

    render(<NetworkPane {...props({ savedAt: 1234 })} />);
    const anchored = document.querySelectorAll('[data-nc-settings-saved]');
    expect(anchored).toHaveLength(1);
    expect(anchored[0]?.textContent).toBe('Saved.');
  });
});
