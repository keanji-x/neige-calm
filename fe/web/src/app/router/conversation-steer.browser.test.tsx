import { cleanup, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, expect, it } from 'vitest';
import { mountSteerFixture, steerAnswer } from './conversation-steer-fixture.tsx';
import '../../styles/entry.css';

afterEach(() => { cleanup(); document.getElementById('root')?.remove(); });
it.each([['codex', true], ['claude', false], ['opencode', false]] as const)(
  'queued %s actions consume declared steer in the real browser', async (provider, steer) => {
    const fixture = mountSteerFixture(provider, () => ({ status: 200, statusText: 'OK', body: steerAnswer(provider, steer) }));
    await userEvent.click(await screen.findByRole('button', { name: /Conversation Daily Planner conversation/ }));
    await screen.findByText('Queued words');
    await waitFor(() => expect(fixture.client.getQueryData(['managed-providers'])).toEqual(steerAnswer(provider, steer)));
    expect(screen.queryByRole('button', { name: 'Say it now' }) !== null).toBe(steer);
    const edit = screen.getByRole('button', { name: 'Edit queued message' });
    expect(screen.getByRole('button', { name: 'Delete this message' })).toBeTruthy();
    edit.focus();
    expect(document.activeElement).toBe(edit);
  });
