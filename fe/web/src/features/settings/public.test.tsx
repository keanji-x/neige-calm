// @vitest-environment jsdom
import { act, cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';

import { HTTPS_PROXY_KEY, HTTP_PROXY_KEY } from '../../../../core/domain/settings.ts';
import { SettingsPage, type SettingsPageProps } from './public.tsx';

afterEach(cleanup);

function props(overrides: Partial<SettingsPageProps> = {}): SettingsPageProps {
  return {
    settings: {},
    loadError: null,
    saving: false,
    saveError: null,
    savedAt: null,
    onSave: vi.fn(),
    onRetryLoad: vi.fn(),
    onOpenToday: vi.fn(),
    themeMode: 'system',
    onThemeModeChange: vi.fn(),
    ...overrides,
  };
}

describe('Settings breadcrumb', () => {
  it('leaves for Today through the callback', async () => {
    const onOpenToday = vi.fn();
    render(<SettingsPage {...props({ onOpenToday })} />);
    await userEvent.click(screen.getByRole('button', { name: 'Today' }));
    expect(onOpenToday).toHaveBeenCalledTimes(1);
  });
});

describe('Settings network form', () => {
  it('seeds the proxy fields from the settings bag', () => {
    render(<SettingsPage {...props({
      settings: { [HTTP_PROXY_KEY]: 'http://box:3128', [HTTPS_PROXY_KEY]: 'http://box:3129' },
    })} />);
    expect(screen.getByLabelText<HTMLInputElement>('HTTP proxy').value).toBe('http://box:3128');
    expect(screen.getByLabelText<HTMLInputElement>('HTTPS proxy').value).toBe('http://box:3129');
  });

  it('shows an empty field when the key is absent from the bag', () => {
    render(<SettingsPage {...props()} />);
    expect(screen.getByLabelText<HTMLInputElement>('HTTP proxy').value).toBe('');
  });

  it('sends the edited value under the domain key', async () => {
    const onSave = vi.fn();
    render(<SettingsPage {...props({ onSave })} />);
    await userEvent.type(screen.getByLabelText('HTTPS proxy'), 'http://edge:8080');
    await userEvent.click(screen.getByRole('button', { name: 'Save' }));
    expect(onSave).toHaveBeenCalledWith({ [HTTPS_PROXY_KEY]: 'http://edge:8080' });
  });

  // CR-6 — in flight the button is *busy*, not `disabled`. A real `disabled`
  // element is not focusable, and focus is on Save at exactly that moment, so
  // disabling it would throw focus to <body> mid-action. The block has to come
  // from `aria-disabled` + the handler, and the state has to stay announceable.
  it('flips the save label and blocks the button while saving, without disabling it', () => {
    render(<SettingsPage {...props({ saving: true })} />);
    const save = screen.getByRole('button', { name: 'Saving…' });
    expect(save.hasAttribute('disabled')).toBe(false);
    expect(save.getAttribute('aria-disabled')).toBe('true');
    expect(save.getAttribute('aria-busy')).toBe('true');
    expect(save.dataset.ncState).toBe('busy');
  });

  it('marks only the ready Save label as visible before saving', async () => {
    render(<SettingsPage {...props()} />);
    await userEvent.type(screen.getByLabelText('HTTP proxy'), 'http://edge');
    const save = screen.getByRole('button', { name: 'Save' });
    const labels = save.querySelectorAll('span > span');
    expect(labels[0]?.getAttribute('aria-hidden')).toBe('false');
    expect(labels[1]?.getAttribute('aria-hidden')).toBe('true');
    expect(save.dataset.ncState).toBeUndefined();
  });

  it('re-seeds the fields when the settings prop reports a new server value', () => {
    const view = render(<SettingsPage {...props({ settings: { [HTTP_PROXY_KEY]: 'http://old' } })} />);
    view.rerender(<SettingsPage {...props({ settings: { [HTTP_PROXY_KEY]: 'http://new' } })} />);
    expect(screen.getByLabelText<HTMLInputElement>('HTTP proxy').value).toBe('http://new');
  });

  it('keeps what the user typed when the parent re-renders with an equal bag', async () => {
    const view = render(<SettingsPage {...props({ settings: { [HTTP_PROXY_KEY]: 'http://box' } })} />);
    await userEvent.clear(screen.getByLabelText('HTTP proxy'));
    await userEvent.type(screen.getByLabelText('HTTP proxy'), 'http://typed');
    // A fresh object with identical values — a query cache does this.
    view.rerender(<SettingsPage {...props({ settings: { [HTTP_PROXY_KEY]: 'http://box' } })} />);
    expect(screen.getByLabelText<HTMLInputElement>('HTTP proxy').value).toBe('http://typed');
  });

  it('preserves a field edited during an in-flight save when the response updates another field', async () => {
    const view = render(<SettingsPage {...props({
      settings: { [HTTP_PROXY_KEY]: 'http://old-http', [HTTPS_PROXY_KEY]: 'http://old-https' },
      saving: true,
    })} />);
    await userEvent.clear(screen.getByLabelText('HTTPS proxy'));
    await userEvent.type(screen.getByLabelText('HTTPS proxy'), 'http://typed-during-save');
    view.rerender(<SettingsPage {...props({
      settings: { [HTTP_PROXY_KEY]: 'http://saved-http', [HTTPS_PROXY_KEY]: 'http://old-https' },
      saving: false, savedAt: 123,
    })} />);
    expect(screen.getByLabelText<HTMLInputElement>('HTTPS proxy').value).toBe('http://typed-during-save');
  });
});

describe('Settings appearance', () => {
  it('moves and selects with radiogroup arrow keys', async () => {
    const onThemeModeChange = vi.fn();
    render(<SettingsPage {...props({ themeMode: 'system', onThemeModeChange })} />);
    const system = screen.getByRole('radio', { name: 'System' });
    system.focus();
    await userEvent.keyboard('{ArrowRight}');
    expect(onThemeModeChange).toHaveBeenCalledWith('light');
    expect(document.activeElement).toBe(screen.getByRole('radio', { name: 'Light' }));
  });
  it('reports the selected mode without going through onSave', async () => {
    const onThemeModeChange = vi.fn();
    const onSave = vi.fn();
    render(<SettingsPage {...props({ onThemeModeChange, onSave })} />);
    await userEvent.click(screen.getByRole('radio', { name: 'Dark' }));
    expect(onThemeModeChange).toHaveBeenCalledWith('dark');
    expect(onSave).not.toHaveBeenCalled();
  });

  it('marks the active mode as checked in the Appearance radiogroup', () => {
    render(<SettingsPage {...props({ themeMode: 'light' })} />);
    const group = screen.getByRole('radiogroup', { name: 'Appearance' });
    expect(group).toBeTruthy();
    expect(screen.getByRole('radio', { name: 'Light' }).getAttribute('aria-checked')).toBe('true');
    expect(screen.getByRole('radio', { name: 'System' }).getAttribute('aria-checked')).toBe('false');
  });
});

describe('Settings states', () => {
  it('surfaces a load failure as an alert', () => {
    render(<SettingsPage {...props({ settings: undefined, loadError: 'settings unreachable' })} />);
    expect(screen.getByRole('alert')).toBeTruthy();
    expect(screen.getByText('settings unreachable')).toBeTruthy();
    expect(screen.queryByText('Loading settings…')).toBeNull();
  });

  it('retries a failed settings read from the in-place error', async () => {
    const onRetryLoad = vi.fn();
    render(<SettingsPage {...props({ settings: undefined, loadError: 'settings unreachable', onRetryLoad })} />);
    await userEvent.click(screen.getByRole('button', { name: 'Retry' }));
    expect(onRetryLoad).toHaveBeenCalledTimes(1);
  });

  it('surfaces a save failure as an alert', () => {
    render(<SettingsPage {...props({ saveError: 'PUT /api/settings failed' })} />);
    expect(screen.getByRole('alert').textContent).toBe('PUT /api/settings failed');
  });

  it('confirms a save through a status role and then retires the notice', () => {
    vi.useFakeTimers();
    try {
      render(<SettingsPage {...props({ savedAt: 1234, savedNoticeMs: 10 })} />);
      expect(screen.getByRole('status').textContent).toBe('Saved.');
      act(() => { vi.advanceTimersByTime(20); });
      expect(screen.queryByRole('status')).toBeNull();
    } finally {
      vi.useRealTimers();
    }
  });
});
