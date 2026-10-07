// The Planner's questions use Astryx’s drawer above the message field.
import { useLayoutEffect, useRef } from 'react';
import { Button } from '@astryxdesign/core/Button';
import { ChatComposerDrawer } from '@astryxdesign/core/Chat';
import { HStack } from '@astryxdesign/core/HStack';
import { Icon as AstryxIcon } from '@astryxdesign/core/Icon';
import { TextInput } from '@astryxdesign/core/TextInput';

import {
  ANSWER_ASK_FAILURES, ANSWER_ASK_TEXT, askAnswers, askDraftsFor, clampAskAnswer, type AskDraft, type OpenAsk,
} from '../../../../core/domain/ask.ts';
import type { AskAnswer } from '../../../../core/api/generated/wire.ts';
import { writeFailureText } from '../../../../core/domain/failure-class.ts';
import { OperationFeedback, useOperationFeedback } from '../../ui/operation-feedback/public.tsx';
import { Icon } from '../../ui/icon/public.tsx';
import { useState } from '../../ui/state/public.ts';
import styles from './asks.module.css';

export type AnswerAsk = (askId: number, answers: readonly AskAnswer[]) => Promise<void>;
export type PlannerAskDrawerProps = Readonly<{ asks: readonly OpenAsk[]; onAnswer: AnswerAsk }>;
type DraftState = Readonly<{ drafts: readonly AskDraft[]; page: number }>;

/** The vendor owns the surface and disclosure; this feature owns question order and answers. */
export function PlannerAskDrawer({ asks, onAnswer }: PlannerAskDrawerProps) {
  const [settled, setSettled] = useState<ReadonlySet<number>>(() => new Set());
  const [states, setStates] = useState<Readonly<Record<number, DraftState>>>({});
  const [selected, setSelected] = useState<number | null>(null);
  const [queueOpen, setQueueOpen] = useState(false);
  const [busy, setBusy] = useState(false);
  const open = asks.filter(ask => !settled.has(ask.askId));
  const current = open.find(ask => ask.askId === selected) ?? open[0];
  if (current === undefined) return null;
  return <ChatComposerDrawer count={current.questions.length} label="Questions" className={styles.drawer}>
    <div className={styles.asks} data-nc-asks="">
      <AskForm key={current.askId} ask={current} onAnswer={onAnswer}
        state={states[current.askId] ?? { drafts: askDraftsFor(current.questions), page: 0 }}
        onChange={state => setStates(previous => ({ ...previous, [current.askId]: state }))}
        onBusy={setBusy}
        onSettled={() => {
          setSettled(previous => new Set(previous).add(current.askId));
          setSelected(null);
          setQueueOpen(false);
        }} />
      {open.length > 1 && <div className={styles.queue}>
        <Button label={`${open.length - 1} more ${open.length === 2 ? 'ask' : 'asks'}`}
          variant="secondary" size="sm" className={styles.queueToggle} isDisabled={busy}
          aria-expanded={queueOpen} onClick={() => setQueueOpen(value => !value)} />
        {queueOpen && <div className={styles.queueList} role="group" aria-label="Other Planner asks">
          {open.filter(ask => ask.askId !== current.askId).map(ask => <Button key={ask.askId}
            label={ask.questions[0].title} variant="secondary" size="sm" className={styles.queueItem}
            isDisabled={busy} onClick={() => { setSelected(ask.askId); setQueueOpen(false); }} />)}
        </div>}
      </div>}
    </div>
  </ChatComposerDrawer>;
}

function AskForm({ ask, state, onChange, onAnswer, onBusy, onSettled }: {
  ask: OpenAsk;
  state: DraftState;
  onChange: (state: DraftState) => void;
  onAnswer: AnswerAsk;
  onBusy: (busy: boolean) => void;
  onSettled: () => void;
}) {
  const feedback = useOperationFeedback();
  const [busy, setBusy] = useState(false);
  const lock = useRef(false);
  const { drafts, page } = state;
  const question = ask.questions[page];
  const draft = drafts[page];
  const firstOption = useRef<HTMLButtonElement | null>(null);
  const ownInput = useRef<HTMLInputElement | null>(null);
  const previousPage = useRef(page);
  useLayoutEffect(() => {
    if (previousPage.current === page) return;
    previousPage.current = page;
    (question.options.length === 0 ? ownInput.current : firstOption.current)?.focus();
  }, [page, question.options.length]);
  const send = async (next: readonly AskDraft[]) => {
    const answers = askAnswers(ask.questions, next);
    if (answers === null || lock.current) return;
    // This form owns the write lifecycle; option onClick keeps its shared lock outside Astryx’s async transition.
    lock.current = true;
    setBusy(true);
    onBusy(true);
    try {
      const success = await feedback.run(onAnswer(ask.askId, answers),
        writeFailureText(ANSWER_ASK_FAILURES, ANSWER_ASK_TEXT));
      if (success) onSettled();
    } finally {
      lock.current = false;
      setBusy(false);
      onBusy(false);
    }
  };
  const advance = async (next: readonly AskDraft[]) => {
    if (lock.current) return;
    onChange({ drafts: next, page });
    feedback.clear();
    if (page < ask.questions.length - 1) {
      onChange({ drafts: next, page: page + 1 });
    } else await send(next);
  };
  const ownAnswer = () => {
    const answer = draft.own.trim();
    if (answer === '' || lock.current) return;
    return advance(drafts.map((item, index) => index === page ? { ...item, own: answer } : item));
  };
  return <section role="group" aria-label="The Planner asks" className={styles.ask} data-nc-ask={ask.askId}>
    {ask.questions.length > 1 && <HStack justify="between" align="center" className={styles.meta}>
      <span>{page + 1} / {ask.questions.length}</span>
      {page > 0 && <Button label="Previous question" tooltip="Previous question" icon={<Icon name="arrow-left" size="sm" />} isIconOnly size="sm" variant="secondary" className={styles.back}
        isDisabled={busy} onClick={() => { feedback.clear(); onChange({ drafts, page: page - 1 }); }} />}
    </HStack>}
    <h3 className={styles.title}>{question.title}</h3>
    <div className={styles.questions} data-nc-ask-questions="">
      {question.options.map((option, index) => <Button key={index} ref={index === 0 ? firstOption : undefined} label={option} variant="secondary"
        className={`${styles.option} ${draft.choice === index && draft.own.trim() === '' ? styles.optionSelected : ''}`}
        aria-pressed={draft.choice === index && draft.own.trim() === ''} isDisabled={busy}
        onClick={() => { void advance(drafts.map((item, at) => at === page ? { choice: index, own: '' } : item)); }}>
        <span className={styles.optionContent}>
          <span className={styles.optionLabel}>{option}</span>
          {draft.choice === index && draft.own.trim() === '' && <AstryxIcon icon="check" size="sm" color="accent" />}
        </span>
      </Button>)}
      {ask.action === undefined && (<div className={styles.own} role="group" aria-label="Your answer">
        <TextInput ref={ownInput} label={question.title} isLabelHidden
          className={styles.answerInput} placeholder={question.options.length > 0 ? 'Or your own answer…' : 'Your answer…'} size="sm"
          value={draft.own} isDisabled={busy}
          onChange={own => onChange({ drafts: drafts.map((item, index) => index === page
            ? { ...item, own: clampAskAnswer(own) } : item), page })} />
        <Button label={ask.questions.length > 1 && page < ask.questions.length - 1 ? 'Next question' : 'Answer'}
          icon={<Icon name="arrow-up" size="sm" />} isIconOnly variant="secondary" size="sm" className={styles.answerButton}
          isDisabled={busy || draft.own.trim() === ''} onClick={() => { void ownAnswer(); }} />
      </div>)}
    </div>
    {busy && <span className={styles.meta} role="status">Sending answer…</span>}
    <OperationFeedback feedback={feedback} />
  </section>;
}
