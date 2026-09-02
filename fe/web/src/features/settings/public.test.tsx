// @vitest-environment jsdom
import { cleanup, render, screen, waitFor } from '@testing-library/react';
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

  it('commits the edited value when the field loses focus', async () => {
    const onSave = vi.fn();
    render(<NetworkPane {...props({ onSave })} />);
    await userEvent.type(screen.getByLabelText('HTTPS proxy'), 'http://edge:8080');
    expect(onSave).not.toHaveBeenCalled();
    await userEvent.tab();
    expect(onSave).toHaveBeenCalledWith({ [HTTPS_PROXY_KEY]: 'http://edge:8080' });
  });

  it('commits on Enter without waiting for the field to be left', async () => {
    const onSave = vi.fn();
    render(<NetworkPane {...props({ onSave })} />);
    await userEvent.type(screen.getByLabelText('HTTP proxy'), 'http://edge:3128{Enter}');
    expect(onSave).toHaveBeenCalledWith({ [HTTP_PROXY_KEY]: 'http://edge:3128' });
  });

  it('does not write while the reader is still typing', async () => {
    const onSave = vi.fn();
    render(<NetworkPane {...props({ onSave })} />);
    // Per-keystroke saving would PUT `h`, `ht`, `htt`… and leave whatever the
    // reader stopped at as the workspace's proxy.
    await userEvent.type(screen.getByLabelText('HTTP proxy'), 'http://edge');
    expect(onSave).not.toHaveBeenCalled();
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

  it('surfaces a save failure on the row that failed, keeping what was typed', async () => {
    const view = render(<NetworkPane {...props({ onSave: vi.fn() })} />);
    await userEvent.type(screen.getByLabelText('HTTP proxy'), 'http://edge:3128');
    await userEvent.tab();
    view.rerender(<NetworkPane {...props({ saveError: 'PUT /api/settings failed' })} />);
    expect(screen.getByRole('alert').textContent).toContain('PUT /api/settings failed');
    expect(screen.getByLabelText<HTMLInputElement>('HTTP proxy').value).toBe('http://edge:3128');
  });

  it('confirms the commit on its own row and then retires the notice', async () => {
    const view = render(<NetworkPane {...props({ onSave: vi.fn(), savedNoticeMs: 10 })} />);
    await userEvent.type(screen.getByLabelText('HTTPS proxy'), 'http://edge:8080');
    await userEvent.tab();
    view.rerender(<NetworkPane {...props({ savedAt: 1234, savedNoticeMs: 10 })} />);
    // astryx renders a success status as `role="status"`; this pane has no
    // buttons, so nothing else on it claims that role.
    expect(screen.getByRole('status').textContent).toContain('Saved.');
    await waitFor(() => expect(screen.queryByRole('status')).toBeNull());
  });

  it('shows no confirmation for a field the reader never committed', () => {
    render(<NetworkPane {...props({ savedAt: 1234 })} />);
    // A `savedAt` from someone else's commit must not decorate a row here.
    expect(screen.queryByRole('status')).toBeNull();
  });
});

