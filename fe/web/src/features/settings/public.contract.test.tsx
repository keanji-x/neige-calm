// @vitest-environment jsdom
// Invariants for the Settings surface. Behavior lives in public.test.tsx.
import { cleanup, render, screen } from '@testing-library/react';
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
    onOpenToday: vi.fn(),
    themeMode: 'system',
    onThemeModeChange: vi.fn(),
    ...overrides,
  };
}

describe('INV-SETTINGS-001 the save patch states intent', () => {
  it('keeps Save disabled until a field actually changes, and disables it again after Reset', async () => {
    render(<SettingsPage {...props({ settings: { [HTTP_PROXY_KEY]: 'http://box:3128' } })} />);
    expect(screen.getByRole('button', { name: 'Save' }).hasAttribute('disabled')).toBe(true);

    await userEvent.type(screen.getByLabelText('HTTP proxy'), '9');
    expect(screen.getByRole('button', { name: 'Save' }).hasAttribute('disabled')).toBe(false);

    await userEvent.click(screen.getByRole('button', { name: 'Reset' }));
    expect(screen.getByLabelText<HTMLInputElement>('HTTP proxy').value).toBe('http://box:3128');
    expect(screen.getByRole('button', { name: 'Save' }).hasAttribute('disabled')).toBe(true);
  });

  it('sends null — not an empty string — for a proxy field the user cleared', async () => {
    const onSave = vi.fn();
    render(<SettingsPage {...props({ onSave, settings: { [HTTP_PROXY_KEY]: 'http://box:3128' } })} />);
    await userEvent.clear(screen.getByLabelText('HTTP proxy'));
    await userEvent.click(screen.getByRole('button', { name: 'Save' }));
    expect(onSave).toHaveBeenCalledWith({ [HTTP_PROXY_KEY]: null });
  });

  it('omits an unchanged key from the patch instead of resending its current value', async () => {
    const onSave = vi.fn();
    render(<SettingsPage {...props({
      onSave,
      settings: { [HTTP_PROXY_KEY]: 'http://box:3128', [HTTPS_PROXY_KEY]: 'http://box:3129' },
    })} />);
    await userEvent.type(screen.getByLabelText('HTTPS proxy'), '9');
    await userEvent.click(screen.getByRole('button', { name: 'Save' }));
    expect(onSave).toHaveBeenCalledWith({ [HTTPS_PROXY_KEY]: 'http://box:31299' });
  });
});

describe('INV-SETTINGS-002 loading never shows an empty form', () => {
  it('renders no text input at all while the settings bag is undefined', () => {
    render(<SettingsPage {...props({ settings: undefined })} />);
    // An empty form here would let the user save blanks over real values.
    expect(screen.queryAllByRole('textbox').length).toBe(0);
    expect(screen.getByText('Loading settings…')).toBeTruthy();
  });
});

describe('INV-A11Y-061 navigation shape', () => {
  it('routes the breadcrumb through a button, never a native link', () => {
    const { container } = render(<SettingsPage {...props({ savedAt: 1, saveError: 'x', loadError: 'y' })} />);
    expect(container.querySelectorAll('a').length).toBe(0);
    expect(screen.getByRole('button', { name: 'Today' }).tagName).toBe('BUTTON');
  });
});
