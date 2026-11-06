// @vitest-environment jsdom
import { act, cleanup, render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, expect, it, vi } from 'vitest';
import type { ModelCatalog } from '../../../../core/domain/conversation.ts';
import { WorkersPane, type WorkersPaneProps, type WorkerDefaultsSaveResult } from './workers.tsx';

afterEach(cleanup);
function props(overrides: Partial<WorkersPaneProps> = {}): WorkersPaneProps {
  return { loaded: true, readRevision: 1, loadError: null, onRetryLoad: vi.fn(), onSave: vi.fn().mockResolvedValue({ ok: true }),
    providers: ['codex', 'claude'].map(provider => ({ provider: provider as 'codex' | 'claude', value: undefined,
      catalog: undefined, catalogError: null, onRetryCatalog: vi.fn() })), ...overrides };
}
function stored(value: string | undefined, overrides: Partial<WorkersPaneProps> = {}): WorkersPaneProps {
  const p = props(overrides);
  return { ...p, providers: p.providers.map(entry => ({ ...entry, value: entry.provider === 'codex' ? value : undefined })) };
}
function deferred() {
  let resolve!: (value: WorkerDefaultsSaveResult) => void;
  return { promise: new Promise<WorkerDefaultsSaveResult>(done => { resolve = done; }), resolve: (value: WorkerDefaultsSaveResult) => resolve(value) };
}
const codex = () => within(screen.getByRole('group', { name: 'Codex defaults' }));

it('keeps each provider draft local across typing, Enter, blur and catalog field changes until explicit Save', async () => {
  const onSave = vi.fn().mockResolvedValue({ ok: true });
  render(<WorkersPane {...props({ onSave })} />);
  await userEvent.type(screen.getByLabelText('Codex model'), 'custom/model{Enter}');
  await userEvent.type(screen.getByLabelText('Codex effort'), 'future');
  await userEvent.type(screen.getByLabelText('Claude model'), 'other');
  await userEvent.tab();
  expect(onSave).not.toHaveBeenCalled();
  await userEvent.click(screen.getByRole('button', { name: 'Save Codex defaults' }));
  expect(onSave).toHaveBeenCalledExactlyOnceWith('codex', { model: 'custom/model', effort: 'future' });
  expect((screen.getByLabelText<HTMLInputElement>('Claude model')).value).toBe('other');
  await userEvent.click(screen.getByRole('button', { name: 'Save Claude defaults' }));
  expect(onSave).toHaveBeenLastCalledWith('claude', { model: 'other', effort: null });
});

it('saves blank members as null and clears the entire provider only on Clear defaults', async () => {
  const onSave = vi.fn().mockResolvedValue({ ok: true });
  render(<WorkersPane {...props({ onSave })} />);
  await userEvent.click(screen.getByRole('button', { name: 'Save Codex defaults' }));
  expect(onSave).toHaveBeenLastCalledWith('codex', { model: null, effort: null });
  await userEvent.click(screen.getByRole('button', { name: 'Clear Codex defaults' }));
  expect(onSave).toHaveBeenLastCalledWith('codex', null);
});

it('does not offer editors before loading or a write capability to a non-owner', async () => {
  const onSave = vi.fn();
  const view = render(<WorkersPane {...props({ loaded: false, onSave })} />);
  expect(screen.queryAllByRole('textbox')).toHaveLength(0);
  view.rerender(<WorkersPane {...props({ onSave: null })} />);
  expect((screen.getByLabelText<HTMLInputElement>('Codex model')).disabled).toBe(true);
  await userEvent.click(screen.getByRole('button', { name: 'Save Codex defaults' }));
  expect(onSave).not.toHaveBeenCalled();
});

it('shows corrupt storage verbatim and clears it without silently saving empty defaults', async () => {
  const onSave = vi.fn().mockResolvedValue({ ok: true });
  render(<WorkersPane {...stored('{"model":"missing effort"}', { onSave })} />);
  expect(codex().getByRole('alert').textContent).toContain('Stored Codex defaults are invalid');
  expect(codex().getByText('{"model":"missing effort"}')).toBeTruthy();
  expect((screen.getByRole<HTMLButtonElement>('button', { name: 'Save Codex defaults' })).disabled).toBe(true);
  await userEvent.click(screen.getByRole('button', { name: 'Clear Codex defaults' }));
  expect(onSave).toHaveBeenCalledExactlyOnceWith('codex', null);
});

it('leaves arbitrary manual slugs editable when the catalog fails and validates syntax', async () => {
  const onSave = vi.fn().mockResolvedValue({ ok: true });
  const p = props({ onSave });
  render(<WorkersPane {...p} providers={p.providers.map(entry => ({ ...entry, catalogError: 'Catalog unavailable; manual configuration is available.' }))} />);
  await userEvent.type(screen.getByLabelText('Codex model'), ' private');
  expect((screen.getByRole<HTMLButtonElement>('button', { name: 'Save Codex defaults' })).disabled).toBe(true);
  await userEvent.clear(screen.getByLabelText('Codex model'));
  await userEvent.type(screen.getByLabelText('Codex model'), 'private');
  await userEvent.click(screen.getByRole('button', { name: 'Save Codex defaults' }));
  expect(onSave).toHaveBeenCalledExactlyOnceWith('codex', { model: 'private', effort: null });
});

it('takes model and effort suggestions from the supplied catalog without committing', async () => {
  const onSave = vi.fn().mockResolvedValue({ ok: true });
  const catalog: ModelCatalog = { source: 'live', default_source: 'unknown', fetched_at_ms: null,
    default: { model: null, reasoning_effort: null, supported_reasoning_efforts: null },
    models: [{ id: 'id', model: 'catalog-model', display_name: 'Catalog model', description: '', resolved_model: null,
      is_default: false, default_reasoning_effort: null, supported_reasoning_efforts: [{ reasoning_effort: 'catalog-effort', description: null }] }] };
  const p = props({ onSave });
  render(<WorkersPane {...p} providers={p.providers.map(entry => ({ ...entry, catalog }))} />);
  await userEvent.click(screen.getByRole('combobox', { name: 'Codex model catalog' }));
  await userEvent.click(screen.getByRole('option', { name: 'Catalog model' }));
  await userEvent.click(screen.getByRole('combobox', { name: 'Codex effort catalog' }));
  await userEvent.click(screen.getByRole('option', { name: 'catalog-effort' }));
  expect(onSave).not.toHaveBeenCalled();
  await userEvent.click(screen.getByRole('button', { name: 'Save Codex defaults' }));
  expect(onSave).toHaveBeenCalledExactlyOnceWith('codex', { model: 'catalog-model', effort: 'catalog-effort' });
});

it.each([false, true])('a late %s clear/save acknowledgement never overwrites or confirms a newer draft, including edit-away-and-back', async clear => {
  const request = deferred();
  const onSave = vi.fn(() => request.promise);
  render(<WorkersPane {...stored('{"model":"old","effort":null}', { onSave })} />);
  await userEvent.click(screen.getByRole('button', { name: clear ? 'Clear Codex defaults' : 'Save Codex defaults' }));
  await userEvent.type(screen.getByLabelText('Codex model'), 'x{Backspace}');
  await act(async () => { request.resolve({ ok: true }); await request.promise; });
  expect((screen.getByLabelText<HTMLInputElement>('Codex model')).value).toBe('old');
  expect(codex().queryByText('Saved.')).toBeNull();
  expect(codex().getByText('Your edit is not saved.')).toBeTruthy();
  expect(onSave).toHaveBeenCalledTimes(1);
});

it('keeps a failed draft and never retries it on blur, Enter or refreshed reads', async () => {
  const onSave = vi.fn().mockResolvedValue({ ok: false, message: 'The save is unconfirmed.' });
  const p = props({ onSave });
  const view = render(<WorkersPane {...p} />);
  await userEvent.type(screen.getByLabelText('Codex model'), 'private');
  await userEvent.click(screen.getByRole('button', { name: 'Save Codex defaults' }));
  view.rerender(<WorkersPane {...p} readRevision={2} />);
  await userEvent.click(screen.getByLabelText('Codex model'));
  await userEvent.keyboard('{Enter}{Tab}');
  expect(codex().getByRole('alert').textContent).toContain('unconfirmed');
  expect((screen.getByLabelText<HTMLInputElement>('Codex model')).value).toBe('private');
  expect(onSave).toHaveBeenCalledTimes(1);
  await userEvent.click(screen.getByRole('button', { name: 'Save Codex defaults' }));
  expect(onSave).toHaveBeenCalledTimes(2);
});

it('uses read revisions to reject an obsolete success even when a newer GET returns the same original value', async () => {
  const request = deferred();
  const p = stored('{"model":"original","effort":null}', { onSave: () => request.promise });
  const view = render(<WorkersPane {...p} />);
  await userEvent.clear(screen.getByLabelText('Codex model'));
  await userEvent.type(screen.getByLabelText('Codex model'), 'submitted');
  await userEvent.click(screen.getByRole('button', { name: 'Save Codex defaults' }));
  view.rerender(<WorkersPane {...p} readRevision={2} />);
  await act(async () => { request.resolve({ ok: true }); await request.promise; });
  expect(codex().queryByText('Saved.')).toBeNull();
  expect(codex().getByText(/Changed elsewhere/)).toBeTruthy();
  expect((screen.getByLabelText<HTMLInputElement>('Codex model')).value).toBe('submitted');
});

it('seeds clean fields from new reads but preserves dirty fields and withdraws superseded confirmations', async () => {
  const p = props();
  const view = render(<WorkersPane {...p} />);
  view.rerender(<WorkersPane {...stored('{"model":"remote","effort":"remote-effort"}', { readRevision: 2 })} />);
  expect((screen.getByLabelText<HTMLInputElement>('Codex model')).value).toBe('remote');
  await userEvent.type(screen.getByLabelText('Codex model'), '-draft');
  view.rerender(<WorkersPane {...stored('{"model":"new-remote","effort":null}', { readRevision: 3 })} />);
  expect((screen.getByLabelText<HTMLInputElement>('Codex model')).value).toBe('remote-draft');
  expect(codex().getByText(/Changed elsewhere/)).toBeTruthy();
  await userEvent.click(screen.getByRole('button', { name: 'Save Codex defaults' }));
  await waitFor(() => expect(codex().getByText('Saved.')).toBeTruthy());
  view.rerender(<WorkersPane {...stored('{"model":"later","effort":null}', { readRevision: 4 })} />);
  expect(codex().queryByText('Saved.')).toBeNull();
  expect((screen.getByLabelText<HTMLInputElement>('Codex model')).value).toBe('remote-draft');
});

it('keeps a failed clear visible after a refresh even when the fields were untouched', async () => {
  const onSave = vi.fn().mockResolvedValue({ ok: false, message: 'The save is unconfirmed.' });
  const p = stored('{"model":"stored","effort":null}', { onSave });
  const view = render(<WorkersPane {...p} />);
  await userEvent.click(screen.getByRole('button', { name: 'Clear Codex defaults' }));
  view.rerender(<WorkersPane {...p} readRevision={2} />);
  expect(codex().getByRole('alert').textContent).toContain('unconfirmed');
  expect(screen.getByLabelText<HTMLInputElement>('Codex model').value).toBe('stored');
  expect(onSave).toHaveBeenCalledTimes(1);
});
