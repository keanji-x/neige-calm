// @vitest-environment jsdom
import { cleanup, render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';

import type { OpenAsk } from '../../../../core/domain/ask.ts';
import { ApiError } from '../../../../core/domain/failure-class.ts';
import { PlannerAskDrawer, type AnswerAsk } from './asks.tsx';

afterEach(cleanup);

const TWO_QUESTIONS: OpenAsk = {
  askId: 7,
  questions: [
    { title: 'Which branch should I release from?', options: ['main', 'release/2.0'] },
    { title: 'Anything to tell the reviewers?', options: [] },
  ],
};
const LATER: OpenAsk = { askId: 9, questions: [{ title: 'Merge PR #12?', options: ['Merge', 'Hold'] }] };

function http(status: number, message: string): ApiError {
  return new ApiError({ kind: 'http', status, code: 'error', message });
}

function renderDrawer(asks: readonly OpenAsk[], onAnswer: AnswerAsk) {
  return render(<PlannerAskDrawer asks={asks} onAnswer={onAnswer} />);
}

/** Astryx keeps a tooltipped disabled button focusable: `aria-disabled`, not `disabled`. */
function isDisabled(button: HTMLElement): boolean {
  return button.hasAttribute('disabled') || button.getAttribute('aria-disabled') === 'true';
}

function answerButton(): HTMLElement {
  return screen.getByRole('button', { name: 'Answer' });
}

describe('PlannerAskDrawer', () => {
  it('renders nothing when the track asks nothing', () => {
    const { container } = renderDrawer([], vi.fn<AnswerAsk>());
    expect(container.innerHTML).toBe('');
  });

  it('shows each question in the Planner’s words, the first option picked and marked recommended', () => {
    renderDrawer([TWO_QUESTIONS], vi.fn<AnswerAsk>());
    const branch = screen.getByRole('radiogroup', { name: 'Which branch should I release from?' });
    expect(within(branch).getByRole<HTMLInputElement>('radio', { name: /main/ }).checked).toBe(true);
    expect(within(branch).getByRole<HTMLInputElement>('radio', { name: 'release/2.0' }).checked).toBe(false);
    expect(within(branch).getByText('Recommended')).toBeTruthy();
    expect(screen.getByRole<HTMLInputElement>('textbox', { name: 'Anything to tell the reviewers?' }).value).toBe('');
    /* The collapse toggle names the questions, not the images beside them. */
    expect(screen.getByRole('button', { name: /Questions/ }).getAttribute('aria-expanded')).toBe('true');
  });

  it('waits for an answer to every question before it can be sent', async () => {
    const onAnswer = vi.fn<AnswerAsk>(() => Promise.resolve());
    renderDrawer([TWO_QUESTIONS], onAnswer);
    expect(isDisabled(answerButton())).toBe(true);
    await userEvent.type(screen.getByRole('textbox', { name: 'Anything to tell the reviewers?' }), '   ');
    expect(isDisabled(answerButton())).toBe(true);
  });

  it('sends the picked option and the free answer, in question order, then hides the ask', async () => {
    const onAnswer = vi.fn<AnswerAsk>(() => Promise.resolve());
    renderDrawer([TWO_QUESTIONS], onAnswer);
    await userEvent.click(screen.getByRole('radio', { name: 'release/2.0' }));
    await userEvent.type(screen.getByRole('textbox', { name: 'Anything to tell the reviewers?' }), ' Ship it ');
    await userEvent.click(answerButton());
    await waitFor(() => expect(onAnswer).toHaveBeenCalledOnce());
    expect(onAnswer).toHaveBeenCalledWith(7, ['release/2.0', 'Ship it']);
    await waitFor(() => expect(screen.queryByRole('group', { name: 'The Planner asks' })).toBeNull());
  });

  it('sends the reader’s own words instead of the picked option once they write some', async () => {
    const onAnswer = vi.fn<AnswerAsk>(() => Promise.resolve());
    renderDrawer([LATER], onAnswer);
    await userEvent.type(screen.getByRole('textbox', { name: 'Your own answer: Merge PR #12?' }), 'Wait for CI');
    await userEvent.click(answerButton());
    await waitFor(() => expect(onAnswer).toHaveBeenCalledWith(9, ['Wait for CI']));
  });

  it('shows the oldest ask first, and the next once it is answered', async () => {
    const onAnswer = vi.fn<AnswerAsk>(() => Promise.resolve());
    renderDrawer([LATER, TWO_QUESTIONS].sort((a, b) => a.askId - b.askId), onAnswer);
    expect(screen.getByText('1 more ask waits after this one.')).toBeTruthy();
    expect(screen.queryByRole('radiogroup', { name: 'Merge PR #12?' })).toBeNull();
    await userEvent.type(screen.getByRole('textbox', { name: 'Anything to tell the reviewers?' }), 'No');
    await userEvent.click(answerButton());
    await screen.findByRole('radiogroup', { name: 'Merge PR #12?' });
    expect(screen.queryByText(/more ask/)).toBeNull();
  });

  it('drops an ask already answered elsewhere (409) without a word', async () => {
    const onAnswer = vi.fn<AnswerAsk>(() => Promise.reject(http(409, 'ask 9 is already answered')));
    renderDrawer([LATER], onAnswer);
    await userEvent.click(answerButton());
    await waitFor(() => expect(screen.queryByRole('group', { name: 'The Planner asks' })).toBeNull());
    expect(screen.queryByRole('alert')).toBeNull();
    expect(screen.queryByText(/already answered/)).toBeNull();
  });

  it.each([
    [400, 'answers[0] must not be empty'],
    [404, 'ask 9 on track w1'],
  ])('keeps the ask and says why when the answer is refused (%i)', async (status, reason) => {
    const onAnswer = vi.fn<AnswerAsk>(() => Promise.reject(http(status, reason)));
    renderDrawer([LATER], onAnswer);
    await userEvent.click(answerButton());
    expect(await screen.findByText(reason)).toBeTruthy();
    expect(screen.getByRole('group', { name: 'The Planner asks' })).toBeTruthy();
    expect(screen.getByRole<HTMLInputElement>('radio', { name: /Merge/ }).checked).toBe(true);
  });

  it('keeps the ask when the outcome is unknown, so Answer can be pressed again', async () => {
    const onAnswer = vi.fn<AnswerAsk>()
      .mockRejectedValueOnce(new ApiError({ kind: 'transport', message: 'dropped' }))
      .mockRejectedValueOnce(http(409, 'ask 9 is already answered'));
    renderDrawer([LATER], onAnswer);
    await userEvent.click(answerButton());
    expect(await screen.findByText('Sending your answer is unconfirmed.')).toBeTruthy();
    await userEvent.click(answerButton());
    await waitFor(() => expect(screen.queryByRole('group', { name: 'The Planner asks' })).toBeNull());
    expect(onAnswer).toHaveBeenCalledTimes(2);
  });
});
