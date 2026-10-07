// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { HOLD_ASK_GONE_TEXT, type OpenAsk } from '../../../../core/domain/ask.ts';
import { ApiError } from '../../../../core/domain/failure-class.ts';
import { PlannerAskDrawer, type AnswerAsk } from './asks.tsx';

afterEach(cleanup);
const TWO: OpenAsk = { askId: 7, delivery: 'wake', questions: [
  { title: 'Which branch?', options: ['main', 'release'] }, { title: 'Notes?', options: [] },
] };
const SINGLE: OpenAsk = { askId: 9, delivery: 'wake', questions: [{ title: 'Merge PR #12?', options: ['Merge', 'Hold'] }] };
/* A paused turn's request (#2348): one question, options only. */
const HOLD: OpenAsk = { askId: 12, delivery: 'hold', questions: [
  { title: 'Run `cargo test` (cwd /work)?', options: ['Allow', 'Allow for this session', 'Deny'] },
] };
const http = (status: number, message: string) => new ApiError({ kind: 'http', status, code: 'error', message });
function setup(asks: readonly OpenAsk[], onAnswer: AnswerAsk = vi.fn(() => Promise.resolve())) {
  const view = render(<PlannerAskDrawer asks={asks} onAnswer={onAnswer} />);
  return { ...view, onAnswer };
}

describe('PlannerAskDrawer', () => {
  it('renders nothing without open questions', () => {
    expect(setup([]).container.innerHTML).toBe('');
  });
  it('uses Astryx’s questions drawer and its collapse disclosure', async () => {
    const { container } = setup([TWO]);
    expect(container.firstElementChild?.className).toContain('astryx-chat-composer-drawer');
    const toggle = screen.getByRole('button', { name: 'Collapse Questions' });
    expect(toggle.getAttribute('aria-expanded')).toBe('true');
    await userEvent.click(toggle);
    expect(screen.getByRole('button', { name: 'Expand Questions' }).getAttribute('aria-expanded')).toBe('false');
  });
  it('answers a single choice directly from the conversation card', async () => {
    const { onAnswer } = setup([SINGLE]);
    await userEvent.click(screen.getByRole('button', { name: 'Hold' }));
    await waitFor(() => expect(onAnswer).toHaveBeenCalledWith(9, [{ option: 1 }]));
    expect(screen.queryByRole('group', { name: 'The Planner asks' })).toBeNull();
  });
  it('commits all answers in order only at the last question', async () => {
    const { onAnswer } = setup([TWO]);
    expect(screen.getByRole('heading', { name: 'Which branch?' })).toBeTruthy();
    expect(screen.queryByRole('textbox', { name: 'Notes?' })).toBeNull();
    await userEvent.click(screen.getByRole('button', { name: 'release' }));
    expect(onAnswer).not.toHaveBeenCalled();
    await userEvent.type(screen.getByRole('textbox', { name: 'Notes?' }), ' Ship it ');
    await userEvent.click(screen.getByRole('button', { name: 'Answer' }));
    await waitFor(() => expect(onAnswer).toHaveBeenCalledWith(7, [{ option: 1 }, { text: 'Ship it' }]));
  });
  it('marks the chosen option when returning, and clears it for a custom answer', async () => {
    setup([TWO]);
    await userEvent.click(screen.getByRole('button', { name: 'release' }));
    await userEvent.click(screen.getByRole('button', { name: 'Previous question' }));
    expect(screen.getByRole('button', { name: 'release' }).getAttribute('aria-pressed')).toBe('true');
    expect(screen.getByRole('button', { name: 'main' }).getAttribute('aria-pressed')).toBe('false');
    await userEvent.type(screen.getByRole('textbox', { name: 'Which branch?' }), 'custom branch');
    expect(screen.getByRole('button', { name: 'release' }).getAttribute('aria-pressed')).toBe('false');
  });
  it('preserves drafts on return to an earlier question', async () => {
    const { onAnswer } = setup([TWO]);
    await userEvent.click(screen.getByRole('button', { name: 'release' }));
    await userEvent.type(screen.getByRole('textbox', { name: 'Notes?' }), 'Keep these notes');
    await userEvent.click(screen.getByRole('button', { name: 'Previous question' }));
    await userEvent.click(screen.getByRole('button', { name: 'main' }));
    expect(screen.getByRole<HTMLInputElement>('textbox', { name: 'Notes?' }).value).toBe('Keep these notes');
    await userEvent.click(screen.getByRole('button', { name: 'Answer' }));
    await waitFor(() => expect(onAnswer).toHaveBeenCalledWith(7, [{ option: 0 }, { text: 'Keep these notes' }]));
  });
  it('accepts the reader’s own words for an option question', async () => {
    const { onAnswer } = setup([SINGLE]);
    await userEvent.type(screen.getByRole('textbox', { name: 'Merge PR #12?' }), 'Wait for CI');
    await userEvent.click(screen.getByRole('button', { name: 'Answer' }));
    await waitFor(() => expect(onAnswer).toHaveBeenCalledWith(9, [{ text: 'Wait for CI' }]));
  });
  it('rejects blank free answers and clamps the server’s character limit', () => {
    setup([SINGLE]);
    const input = screen.getByRole<HTMLInputElement>('textbox', { name: 'Merge PR #12?' });
    fireEvent.change(input, { target: { value: '   ' } });
    expect(screen.getByRole<HTMLButtonElement>('button', { name: 'Answer' }).disabled).toBe(true);
    fireEvent.change(input, { target: { value: 'x'.repeat(2001) } });
    expect(input.value).toHaveLength(2000);
  });
  it('preserves the earlier group’s draft and position across group switches', async () => {
    setup([TWO, SINGLE]);
    await userEvent.click(screen.getByRole('button', { name: 'release' }));
    await userEvent.type(screen.getByRole('textbox', { name: 'Notes?' }), 'Draft');
    await userEvent.click(screen.getByRole('button', { name: '1 more ask' }));
    await userEvent.click(screen.getByRole('button', { name: 'Merge PR #12?' }));
    expect(screen.getByRole('heading', { name: 'Merge PR #12?' })).toBeTruthy();
    await userEvent.click(screen.getByRole('button', { name: '1 more ask' }));
    await userEvent.click(screen.getByRole('button', { name: 'Which branch?' }));
    expect(screen.getByRole<HTMLInputElement>('textbox', { name: 'Notes?' }).value).toBe('Draft');
  });
  it('settles a confirmed ask before a stale overlay catches up', async () => {
    const { rerender, onAnswer } = setup([SINGLE, TWO]);
    await userEvent.click(screen.getByRole('button', { name: 'Hold' }));
    await screen.findByRole('heading', { name: 'Which branch?' });
    rerender(<PlannerAskDrawer asks={[SINGLE, TWO]} onAnswer={onAnswer} />);
    expect(screen.queryByRole('heading', { name: 'Merge PR #12?' })).toBeNull();
  });
  it('settles a 409 without inventing a local answer receipt', async () => {
    setup([SINGLE], () => Promise.reject(http(409, 'already answered')));
    await userEvent.click(screen.getByRole('button', { name: 'Hold' }));
    await waitFor(() => expect(screen.queryByRole('group', { name: 'The Planner asks' })).toBeNull());
    expect(screen.queryByRole('status', { name: 'Submitted Planner answers' })).toBeNull();
    expect(screen.queryByRole('alert')).toBeNull();
  });
  it.each([400, 404])('keeps the question after a refusal (%i)', async status => {
    setup([SINGLE], () => Promise.reject(http(status, 'Not permitted')));
    await userEvent.click(screen.getByRole('button', { name: 'Hold' }));
    expect(await screen.findByText('Not permitted')).toBeTruthy();
    expect(screen.getByRole('heading', { name: 'Merge PR #12?' })).toBeTruthy();
    expect(screen.queryByRole('status', { name: 'Submitted Planner answers' })).toBeNull();
  });
  it('keeps uncertain writes retryable and settles a 409 retry', async () => {
    const onAnswer = vi.fn<AnswerAsk>().mockRejectedValueOnce(new ApiError({ kind: 'transport', message: 'dropped' }))
      .mockRejectedValueOnce(http(409, 'already answered'));
    setup([SINGLE], onAnswer);
    await userEvent.click(screen.getByRole('button', { name: 'Hold' }));
    expect(await screen.findByText('Sending your answer is unconfirmed.')).toBeTruthy();
    await userEvent.click(screen.getByRole('button', { name: 'Hold' }));
    await waitFor(() => expect(screen.queryByRole('group', { name: 'The Planner asks' })).toBeNull());
    expect(onAnswer).toHaveBeenCalledTimes(2);
  });
  it('locks every choice and group switch during a write', async () => {
    let finish!: () => void;
    const onAnswer = vi.fn<AnswerAsk>(() => new Promise<void>(resolve => { finish = resolve; }));
    setup([SINGLE, TWO], onAnswer);
    const hold = screen.getByRole('button', { name: 'Hold' });
    fireEvent.click(hold); fireEvent.click(hold);
    fireEvent.click(screen.getByRole('button', { name: 'Merge' }));
    expect(onAnswer).toHaveBeenCalledOnce();
    const switcher = screen.getByRole<HTMLButtonElement>('button', { name: '1 more ask' });
    expect(switcher.disabled || switcher.getAttribute('aria-disabled') === 'true').toBe(true);
    finish();
    await screen.findByRole('heading', { name: 'Which branch?' });
  });
});

it('offers only the two explicit choices for a lifecycle question, with no ambiguous own-answer input', () => {
  const ask = { ...SINGLE, action: { kind: 'reopen_track' as const, closed_at: 42 },
    questions: [{ title: 'Continue this closed track?', options: ['Reopen and continue', 'Keep closed'] }] };
  setup([ask]);
  expect(screen.queryByRole('textbox', { name: 'Continue this closed track?' })).toBeNull();
  expect(screen.getByRole('button', { name: 'Reopen and continue' })).toBeTruthy();
  expect(screen.getByRole('button', { name: 'Keep closed' })).toBeTruthy();
});

describe('PlannerAskDrawer with a paused turn (#2348)', () => {
  it('says the turn is paused and offers its options only, with no field for words of one\'s own', () => {
    setup([HOLD]);
    const ask = screen.getByRole('group', { name: 'The Planner asks' });
    expect(ask.textContent).toContain('Turn paused, waiting for your approval');
    expect(ask.querySelector('[data-nc-ask-paused]')?.textContent).toBe('Turn paused, waiting for your approval');
    expect(screen.queryByRole('textbox')).toBeNull();
    expect(screen.queryByRole('group', { name: 'Your answer' })).toBeNull();
    expect(screen.queryByRole('button', { name: 'Answer' })).toBeNull();
    expect(screen.queryByRole('button', { name: /dismiss/i })).toBeNull();
    /* An approval is not pre-approved: no option is marked until the reader picks one. */
    for (const name of ['Allow', 'Allow for this session', 'Deny']) {
      expect(screen.getByRole('button', { name }).getAttribute('aria-pressed')).toBe('false');
    }
  });
  it('sends the clicked option by its index', async () => {
    const { onAnswer } = setup([HOLD]);
    await userEvent.click(screen.getByRole('button', { name: 'Deny' }));
    await waitFor(() => expect(onAnswer).toHaveBeenCalledWith(12, [{ option: 2 }]));
    expect(screen.queryByRole('group', { name: 'The Planner asks' })).toBeNull();
  });
  it('says a request that went away is no longer pending, and keeps the row until the overlay drops it', async () => {
    const { rerender, onAnswer } = setup([HOLD], vi.fn<AnswerAsk>(() => Promise.reject(http(409, 'its paused request is gone'))));
    await userEvent.click(screen.getByRole('button', { name: 'Allow' }));
    expect(await screen.findByText(HOLD_ASK_GONE_TEXT)).toBeTruthy();
    expect(screen.queryByText('its paused request is gone')).toBeNull();
    expect(screen.getByRole('heading', { name: 'Run `cargo test` (cwd /work)?' })).toBeTruthy();
    rerender(<PlannerAskDrawer asks={[]} onAnswer={onAnswer} />);
    expect(screen.queryByRole('group', { name: 'The Planner asks' })).toBeNull();
  });
  it('leaves a wake ask beside it as it was', () => {
    setup([HOLD, SINGLE]);
    expect(screen.queryByRole('textbox')).toBeNull();
    fireEvent.click(screen.getByRole('button', { name: '1 more ask' }));
    fireEvent.click(screen.getByRole('button', { name: 'Merge PR #12?' }));
    expect(screen.getByRole('textbox', { name: 'Merge PR #12?' })).toBeTruthy();
    expect(screen.queryByText('Turn paused, waiting for your approval')).toBeNull();
  });
});
