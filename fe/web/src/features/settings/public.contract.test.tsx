// @vitest-environment jsdom
// Invariants for the Settings surface. Behavior lives in public.test.tsx.
import { cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';

import { HTTPS_PROXY_KEY, HTTP_PROXY_KEY } from '../../../../core/domain/settings.ts';
import { NetworkPane, SettingsSurface, type NetworkPaneProps } from './public.tsx';

afterEach(cleanup);

function props(overrides: Partial<NetworkPaneProps> = {}): NetworkPaneProps {
  return {
    settings: {},
    loadError: null,
    onSave: vi.fn(),
    onRetryLoad: vi.fn(),
    ...overrides,
  };
}

describe('INV-SETTINGS-001 the save patch states intent', () => {
  it('writes nothing when a field is entered and left untouched', async () => {
    const onSave = vi.fn();
    render(<NetworkPane {...props({ onSave, settings: { [HTTP_PROXY_KEY]: 'http://box:3128' } })} />);
    // There is no Save button to press: leaving the field is the commit. A
    // field the reader only tabbed through must therefore write nothing.
    await userEvent.click(screen.getByLabelText('HTTP proxy'));
    await userEvent.tab();
    expect(onSave).not.toHaveBeenCalled();
    expect(screen.queryByRole('button', { name: 'Save' })).toBeNull();
  });

  it('sends null — not an empty string — for a proxy field the reader cleared', async () => {
    const onSave = vi.fn();
    render(<NetworkPane {...props({ onSave, settings: { [HTTP_PROXY_KEY]: 'http://box:3128' } })} />);
    await userEvent.clear(screen.getByLabelText('HTTP proxy'));
    await userEvent.tab();
    expect(onSave).toHaveBeenCalledWith({ [HTTP_PROXY_KEY]: null });
  });

  it('commits one key, never the whole form', async () => {
    const onSave = vi.fn();
    render(<NetworkPane {...props({
      onSave,
      settings: { [HTTP_PROXY_KEY]: 'http://box:3128', [HTTPS_PROXY_KEY]: 'http://box:3129' },
    })} />);
    await userEvent.type(screen.getByLabelText('HTTPS proxy'), '9');
    await userEvent.tab();
    // The untouched key is absent, not resent: two tabs editing different keys
    // cannot clobber each other.
    expect(onSave).toHaveBeenCalledWith({ [HTTPS_PROXY_KEY]: 'http://box:31299' });
  });
});

describe('INV-SETTINGS-002 loading never shows an empty form', () => {
  it('renders no text input at all while the settings bag is undefined', () => {
    render(<NetworkPane {...props({ settings: undefined })} />);
    // An empty form here would let the user save blanks over real values.
    expect(screen.queryAllByRole('textbox').length).toBe(0);
    expect(screen.getByText('Loading settings…')).toBeTruthy();
  });
});

describe('INV-A11Y-061 navigation shape', () => {
  it('renders no native link anywhere, in any state', () => {
    const { container } = render(<NetworkPane {...props({ loadError: 'y' })} />);
    expect(container.querySelectorAll('a').length).toBe(0);
  });

  it('names every settings section on a button, and marks the current one', async () => {
    const onSelectSection = vi.fn();
    render(
      <SettingsSurface section="network" onSelectSection={onSelectSection}>
        <span>pane</span>
      </SettingsSurface>,
    );
    const nav = screen.getByRole('navigation', { name: 'Settings sections' });
    expect([...nav.querySelectorAll('button')].map((node) => node.textContent))
      .toEqual(['Network', 'Appearance', 'Plugins', 'About']);
    expect(screen.getByRole('button', { name: 'Network' }).getAttribute('aria-current')).toBe('page');
    expect(screen.getByRole('button', { name: 'Plugins' }).getAttribute('aria-current')).toBeNull();

    await userEvent.click(screen.getByRole('button', { name: 'Plugins' }));
    expect(onSelectSection.mock.calls).toEqual([['plugins']]);
  });
});
