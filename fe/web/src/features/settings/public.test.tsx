// @vitest-environment jsdom
import { act, cleanup, render, screen, waitFor } from '@testing-library/react';
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
    let settle = () => undefined as void;
    const onSave = vi.fn(() => new Promise<void>((resolve) => { settle = resolve; }));
    const view = render(<NetworkPane {...props({
      settings: { [HTTP_PROXY_KEY]: 'http://old-http', [HTTPS_PROXY_KEY]: 'http://old-https' },
      onSave,
    })} />);
    await userEvent.type(screen.getByLabelText('HTTP proxy'), '-edited');
    await userEvent.tab();                       // HTTP in flight
    await userEvent.clear(screen.getByLabelText('HTTPS proxy'));
    await userEvent.type(screen.getByLabelText('HTTPS proxy'), 'http://typed-during-save');
    await act(async () => { settle(); await Promise.resolve(); });
    view.rerender(<NetworkPane {...props({
      settings: { [HTTP_PROXY_KEY]: 'http://old-http-edited', [HTTPS_PROXY_KEY]: 'http://old-https' },
      onSave,
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
    const onSave = vi.fn(() => Promise.reject(new Error('PUT /api/settings failed')));
    render(<NetworkPane {...props({ onSave })} />);
    await userEvent.type(screen.getByLabelText('HTTP proxy'), 'http://edge:3128');
    await act(async () => { await userEvent.tab(); });
    const row = screen.getByLabelText('HTTP proxy').closest('li');
    expect(row?.textContent).toContain('PUT /api/settings failed');
    expect(screen.getByLabelText<HTMLInputElement>('HTTP proxy').value).toBe('http://edge:3128');
    // The other row is untouched: one failed request describes one row.
    expect(screen.getByLabelText('HTTPS proxy').closest('li')?.textContent)
      .not.toContain('PUT /api/settings failed');
  });

  it('confirms the commit on its own row and then retires the notice', async () => {
    const onSave = vi.fn(() => Promise.resolve());
    render(<NetworkPane {...props({ onSave, savedNoticeMs: 10 })} />);
    await userEvent.type(screen.getByLabelText('HTTPS proxy'), 'http://edge:8080');
    await act(async () => { await userEvent.tab(); });
    const row = screen.getByLabelText('HTTPS proxy').closest('li');
    expect(row?.querySelector('[role="status"]')?.textContent).toBe('Saved.');
    await waitFor(() => {
      expect(row?.querySelector('[role="status"]')?.textContent).toBe('');
    });
  });

  it('mounts the live region before it has anything to say', () => {
    render(<NetworkPane {...props()} />);
    /*
     * Always mounted, empty. A live region that arrives in the same mutation as
     * its text is commonly not announced at all, which would leave the tick as
     * the only confirmation — and a tick has no accessible name.
     */
    expect(screen.getAllByRole('status').map((node) => node.textContent)).toEqual(['', '']);
  });
});

type Deferred = { promise: Promise<void>; resolve: () => void; reject: (error: Error) => void };

function deferred(): Deferred {
  let resolve!: () => void;
  let reject!: (error: Error) => void;
  const promise = new Promise<void>((res, rej) => { resolve = res; reject = rej; });
  return { promise, resolve, reject };
}

/*
 * Two rows share one screen but not one status. Every case here was a
 * reproduced defect first: the pane used to read a pane-level
 * `saving`/`saveError`/`savedAt` triple with a single "which row" pointer, and
 * these four sequences are what that shape got wrong.
 */
describe('Settings network commits, per row', () => {
  it('does not paint one row failure on the other row', async () => {
    const flights: Deferred[] = [];
    const onSave = vi.fn(() => { const d = deferred(); flights.push(d); return d.promise; });
    render(<NetworkPane {...props({ onSave })} />);

    await userEvent.type(screen.getByLabelText('HTTP proxy'), 'http://a:1');
    await userEvent.tab();                       // commit A (in flight)
    await userEvent.type(screen.getByLabelText('HTTPS proxy'), 'http://b:2');
    await userEvent.tab();                       // commit B (in flight)
    await act(async () => { flights[1]?.resolve(); await Promise.resolve(); });
    await act(async () => { flights[0]?.reject(new Error('A failed')); await Promise.resolve(); });

    const httpsRow = screen.getByLabelText('HTTPS proxy').closest('li');
    const httpRow = screen.getByLabelText('HTTP proxy').closest('li');
    expect(httpRow?.textContent).toContain('A failed');
    expect(httpsRow?.textContent).not.toContain('A failed');
  });

  it('does not confirm a commit that has not resolved', async () => {
    const flights: Deferred[] = [];
    const onSave = vi.fn(() => { const d = deferred(); flights.push(d); return d.promise; });
    render(<NetworkPane {...props({ onSave })} />);

    await userEvent.type(screen.getByLabelText('HTTP proxy'), 'http://a:1');
    await userEvent.tab();
    await act(async () => { flights[0]?.resolve(); await Promise.resolve(); });
    const httpRow = screen.getByLabelText('HTTP proxy').closest('li');
    expect(httpRow?.querySelector('[role="status"]')?.textContent).toBe('Saved.');

    await userEvent.type(screen.getByLabelText('HTTPS proxy'), 'http://b:2');
    await userEvent.tab();                        // B in flight, unresolved
    const httpsRow = screen.getByLabelText('HTTPS proxy').closest('li');
    expect(httpsRow?.querySelector('[role="status"]')?.textContent).toBe('');
  });

  it('drops a superseded response instead of letting it overwrite the newer one', async () => {
    const flights: Deferred[] = [];
    const onSave = vi.fn(() => { const d = deferred(); flights.push(d); return d.promise; });
    render(<NetworkPane {...props({ onSave })} />);

    const field = screen.getByLabelText('HTTP proxy');
    await userEvent.type(field, 'http://one:1');
    await userEvent.tab();                        // commit 1
    await userEvent.type(field, '2');
    await userEvent.tab();                        // commit 2 supersedes it
    await act(async () => { flights[1]?.resolve(); await Promise.resolve(); });
    await act(async () => { flights[0]?.reject(new Error('stale failure')); await Promise.resolve(); });

    const httpRow = field.closest('li');
    // Both halves: the stale failure is not shown, **and** the newer commit's
    // own outcome is. Asserting only the absence would stay green if the row
    // simply dropped every verdict.
    expect(httpRow?.textContent).not.toContain('stale failure');
    expect(httpRow?.querySelector('[role="status"]')?.textContent).toBe('Saved.');
  });

  it('commits a value the reader restores while the first write is still out', async () => {
    const flights: Deferred[] = [];
    const onSave = vi.fn(() => { const d = deferred(); flights.push(d); return d.promise; });
    render(<NetworkPane {...props({ onSave, settings: { [HTTP_PROXY_KEY]: 'origin' } })} />);

    const field = screen.getByLabelText('HTTP proxy');
    await userEvent.clear(field);
    await userEvent.type(field, 'changed');
    await userEvent.tab();                        // commit "changed", still out
    await userEvent.clear(field);
    await userEvent.type(field, 'origin');
    await userEvent.tab();                        // back to what the server holds
    /*
     * The second commit must go out. Comparing against the server's bag alone
     * would call this a no-op — the value equals what the server last said —
     * and the in-flight `changed` would land as the reader's final answer.
     */
    expect(onSave).toHaveBeenCalledTimes(2);
    expect(onSave).toHaveBeenLastCalledWith({ [HTTP_PROXY_KEY]: 'origin' });
    await act(async () => { flights[0]?.resolve(); flights[1]?.resolve(); await Promise.resolve(); });
  });

  it('lets the reader retry a commit that failed', async () => {
    const onSave = vi.fn()
      .mockImplementationOnce(() => Promise.reject(new Error('unreachable')))
      .mockImplementationOnce(() => Promise.resolve());
    render(<NetworkPane {...props({ onSave })} />);
    const field = screen.getByLabelText('HTTP proxy');
    await userEvent.type(field, 'http://a:1');
    await act(async () => { await userEvent.tab(); });
    expect(field.closest('li')?.textContent).toContain('unreachable');

    // The obvious retry: focus it again and press Enter. `sent` records what
    // the server *took*, so a failed value must not sit in it and swallow this.
    await act(async () => { await userEvent.type(field, '{Enter}'); });
    expect(onSave).toHaveBeenCalledTimes(2);
    expect(field.closest('li')?.querySelector('[role="status"]')?.textContent).toBe('Saved.');
  });

  it('does not attach a verdict to a value the reader has since changed', async () => {
    const flight = deferred();
    const onSave = vi.fn(() => flight.promise);
    render(<NetworkPane {...props({ onSave })} />);
    const field = screen.getByLabelText('HTTP proxy');
    await userEvent.type(field, 'http://a:1');
    await userEvent.tab();                       // commit A, still out
    await userEvent.type(field, '-more');        // the reader moves on
    await act(async () => { flight.resolve(); await Promise.resolve(); });

    /* A settled at the same moment, and a tick beside `-more` would say the
       value on screen is the one the server took. It is not. */
    expect(field.closest('li')?.querySelector('[role="status"]')?.textContent).toBe('');
    expect(field.closest('li')?.querySelector('svg')).toBeNull();
  });

  it('sends a value the reader restores after an *earlier* write has re-seeded it', async () => {
    const flights: Deferred[] = [];
    const onSave = vi.fn(() => { const d = deferred(); flights.push(d); return d.promise; });
    const view = render(<NetworkPane {...props({ onSave, settings: { [HTTP_PROXY_KEY]: 'A' } })} />);
    const field = screen.getByLabelText('HTTP proxy');

    await userEvent.clear(field);
    await userEvent.type(field, 'B');
    await userEvent.tab();                        // commit B
    await userEvent.clear(field);
    await userEvent.type(field, 'C');
    await userEvent.tab();                        // commit C, still out
    await act(async () => { flights[0]?.resolve(); await Promise.resolve(); });
    // B's bag comes back and re-seeds the row while C is still in flight.
    view.rerender(<NetworkPane {...props({ onSave, settings: { [HTTP_PROXY_KEY]: 'B' } })} />);
    await userEvent.clear(field);
    await userEvent.type(field, 'B');
    await userEvent.tab();

    /*
     * The reader's last word is B, and C is still on its way. Clearing the
     * whole `sent` record on any re-seed made this look unchanged — B equals
     * the bag — so nothing was sent and C landed as the final value.
     */
    expect(onSave).toHaveBeenCalledTimes(3);
    expect(onSave).toHaveBeenLastCalledWith({ [HTTP_PROXY_KEY]: 'B' });
    await act(async () => { flights[1]?.resolve(); await Promise.resolve(); });
  });

  it('does not resend an in-flight value when the other field re-seeds', async () => {
    const flight = deferred();
    const onSave = vi.fn(() => flight.promise);
    const view = render(<NetworkPane {...props({ onSave, settings: {} })} />);
    await userEvent.type(screen.getByLabelText('HTTP proxy'), 'http://a:1');
    await userEvent.tab();                       // HTTP in flight
    // A bag arrives that changes only HTTPS — someone else's write.
    view.rerender(<NetworkPane {...props({ onSave, settings: { [HTTPS_PROXY_KEY]: 'http://b:2' } })} />);
    await userEvent.click(screen.getByLabelText('HTTP proxy'));
    await userEvent.tab();
    expect(onSave).toHaveBeenCalledTimes(1);
    view.unmount();
    expect(onSave).toHaveBeenCalledTimes(1);
  });

  it('withdraws a row verdict as soon as the reader edits it again', async () => {
    const onSave = vi.fn(() => Promise.resolve());
    render(<NetworkPane {...props({ onSave })} />);
    const field = screen.getByLabelText('HTTP proxy');
    await userEvent.type(field, 'http://a:1');
    await act(async () => { await userEvent.tab(); });
    expect(field.closest('li')?.querySelector('[role="status"]')?.textContent).toBe('Saved.');

    await userEvent.type(field, '2');
    // A tick beside a value that was never sent is a lie about the value the
    // reader is looking at.
    expect(field.closest('li')?.querySelector('[role="status"]')?.textContent).toBe('');
  });

  it('does not let an older failure clear the reference for a newer in-flight value', async () => {
    const flights: Deferred[] = [];
    const onSave = vi.fn(() => { const d = deferred(); flights.push(d); return d.promise; });
    const view = render(<NetworkPane {...props({ onSave })} />);
    const field = screen.getByLabelText('HTTP proxy');
    await userEvent.type(field, 'A');
    await userEvent.tab();                       // commit A, ticket 1
    await userEvent.type(field, 'B');
    await userEvent.tab();                       // commit AB, ticket 2, still out
    await act(async () => { flights[0]?.reject(new Error('older failed')); await Promise.resolve(); });

    /*
     * A's failure is superseded and says nothing about the value now in the
     * field. Rolling the reference back on it would make the still-in-flight
     * value look unsent, and the next blur — or the close below — would send
     * it a second time.
     */
    await userEvent.click(field);
    await act(async () => { await userEvent.tab(); });
    expect(onSave).toHaveBeenCalledTimes(2);
    view.unmount();
    expect(onSave).toHaveBeenCalledTimes(2);
  });

  it('does not silently retry, on close, a value the reader watched fail', async () => {
    const onSave = vi.fn(() => Promise.reject(new Error('unreachable')));
    const view = render(<NetworkPane {...props({ onSave, settings: { [HTTP_PROXY_KEY]: 'B' } })} />);
    const field = screen.getByLabelText('HTTP proxy');
    await userEvent.clear(field);
    await userEvent.type(field, 'X');
    await act(async () => { await userEvent.tab(); });
    expect(field.closest('li')?.textContent).toContain('unreachable');

    view.unmount();
    /*
     * Clearing the reference on failure is what makes an explicit retry work.
     * It must not also make *closing* a retry: the reader was told the write
     * did not happen, and a second attempt landing afterwards — with nothing
     * mounted to say so — makes that a lie.
     */
    expect(onSave).toHaveBeenCalledTimes(1);
  });

  it('does not bring a withdrawn verdict back when the draft returns to its value', async () => {
    const onSave = vi.fn(() => Promise.reject(new Error('unreachable')));
    render(<NetworkPane {...props({ onSave })} />);
    const field = screen.getByLabelText('HTTP proxy');
    await userEvent.type(field, 'X');
    await act(async () => { await userEvent.tab(); });
    expect(field.closest('li')?.textContent).toContain('unreachable');

    await userEvent.type(field, 'Y');            // moves away — verdict withdrawn
    await userEvent.keyboard('{Backspace}');     // …and back to exactly X
    // The old message must not reappear: nothing has been sent since.
    expect(field.closest('li')?.textContent).not.toContain('unreachable');
  });

  it('keeps a reference the server never echoes from silencing a later edit', async () => {
    const onSave = vi.fn(() => Promise.resolve());
    const view = render(<NetworkPane {...props({ onSave, settings: { [HTTP_PROXY_KEY]: 'A' } })} />);
    const field = screen.getByLabelText('HTTP proxy');
    await userEvent.clear(field);
    await userEvent.type(field, 'X');
    await act(async () => { await userEvent.tab(); });          // commit X, succeeds

    /*
     * The bag comes back **normalised** — the server stored `X/`, not `X`. A
     * reference kept until the bag equals what was sent would never clear, and
     * would then outrank every future bag: retyping `X` becomes a silent
     * no-op forever.
     */
    view.rerender(<NetworkPane {...props({ onSave, settings: { [HTTP_PROXY_KEY]: 'X/' } })} />);
    await userEvent.clear(field);
    await userEvent.type(field, 'X');
    await act(async () => { await userEvent.tab(); });
    expect(onSave).toHaveBeenCalledTimes(2);
    expect(onSave).toHaveBeenLastCalledWith({ [HTTP_PROXY_KEY]: 'X' });
  });

  it('commits an edit the reader leaves by closing the pane', async () => {
    const onSave = vi.fn(() => Promise.resolve());
    const view = render(<NetworkPane {...props({ onSave })} />);
    await userEvent.type(screen.getByLabelText('HTTP proxy'), 'http://typed:9');
    view.unmount();     // Escape / backdrop close unmounts a focused input: no blur fires.
    expect(onSave.mock.calls).toEqual([[{ [HTTP_PROXY_KEY]: 'http://typed:9' }]]);
  });

  it('does not write twice when the field blurs and the pane then closes', async () => {
    const onSave = vi.fn(() => new Promise<void>(() => undefined));   // never settles
    const view = render(<NetworkPane {...props({ onSave })} />);
    await userEvent.type(screen.getByLabelText('HTTP proxy'), 'http://typed:9');
    await userEvent.tab();          // the real sequence: blur commits…
    view.unmount();                 // …and only then does the dialog go away.
    /*
     * The count is the assertion. `toHaveBeenCalledWith` accepts a duplicate
     * happily, and the duplicate is the defect: the cleanup used to compare the
     * draft against the server's bag, which the in-flight write has not
     * reached yet, so it sent the identical patch a second time.
     */
    expect(onSave).toHaveBeenCalledTimes(1);
  });
});
