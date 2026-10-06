// The Planner's open questions, answered where the reader already talks to it: a drawer above the
// Planner composer (#2209). The questions come from the kernel's activity overlay; the answer is one
// write, and the ask leaves the overlay when the projector's `overlay.set` lands.

import { Badge } from '@astryxdesign/core/Badge';
import { Button } from '@astryxdesign/core/Button';
import { ChatComposerDrawer } from '@astryxdesign/core/Chat';
import { HStack } from '@astryxdesign/core/HStack';
import { RadioList, RadioListItem } from '@astryxdesign/core/RadioList';
import { Text } from '@astryxdesign/core/Text';
import { TextInput } from '@astryxdesign/core/TextInput';
import { VStack } from '@astryxdesign/core/VStack';

import type { AskQuestion } from '../../../../core/domain/activity.ts';
import {
  ANSWER_ASK_FAILURES, ANSWER_ASK_TEXT, askAnswers, askDraftsFor, clampAskAnswer, type AskDraft, type OpenAsk,
} from '../../../../core/domain/ask.ts';
import { writeFailureText } from '../../../../core/domain/failure-class.ts';
import { OperationFeedback, useOperationFeedback } from '../../ui/operation-feedback/public.tsx';
import { useState } from '../../ui/state/public.ts';
import styles from './asks.module.css';

/** Answer one ask, one answer per question in order. Rejects as the write does; the drawer reads it through `ANSWER_ASK_FAILURES`. */
export type AnswerAsk = (askId: number, answers: readonly string[]) => Promise<void>;

export type PlannerAskDrawerProps = Readonly<{
  /** The track's open asks, oldest first (`openAsksOf`). */
  asks: readonly OpenAsk[];
  onAnswer: AnswerAsk;
}>;

/**
 * Its own `ChatComposerDrawer` beside the images', not a section of one: a drawer's `label` names
 * its one disclosure, so each kind of content keeps its own name, count and collapse.
 *
 * One ask at a time, the oldest: the drawer sits over the field on a phone, and the Planner reads
 * its answers in the order it asked. An ask answered here (204) or found already answered (409) is
 * hidden at once rather than when the overlay catches up: an ask that is answered never opens again,
 * so hiding it can never hide one the kernel still lists as open.
 */
export function PlannerAskDrawer({ asks, onAnswer }: PlannerAskDrawerProps) {
  const [settled, setSettled] = useState<ReadonlySet<number>>(() => new Set());
  const open = asks.filter((ask) => !settled.has(ask.askId));
  const current = open[0];
  if (current === undefined) return null;
  const waiting = open.length - 1;
  const settle = (askId: number) => setSettled((previous) => new Set(previous).add(askId));
  return (
    /* The count is the shown ask's questions; the asks after it are the line below. */
    <ChatComposerDrawer count={current.questions.length} label="Questions">
      <VStack gap={2} className={styles.asks} data-nc-asks="">
        <AskForm key={current.askId} ask={current} onAnswer={onAnswer} onSettled={() => settle(current.askId)} />
        {waiting > 0 && (
          <Text as="p" type="supporting" data-nc-asks-waiting="">
            {`${waiting} more ${waiting === 1 ? 'ask waits' : 'asks wait'} after this one.`}
          </Text>
        )}
      </VStack>
    </ChatComposerDrawer>
  );
}

function AskForm({ ask, onAnswer, onSettled }: {
  ask: OpenAsk;
  onAnswer: AnswerAsk;
  onSettled: () => void;
}) {
  const [drafts, setDrafts] = useState<readonly AskDraft[]>(() => askDraftsFor(ask.questions));
  const feedback = useOperationFeedback();
  const answers = askAnswers(ask.questions, drafts);
  const edit = (index: number, change: Partial<AskDraft>) => {
    setDrafts((current) => current.map((draft, at) => at === index ? { ...draft, ...change } : draft));
  };
  return (
    <div role="group" aria-label="The Planner asks" className={styles.ask} data-nc-ask={ask.askId}>
      <VStack gap={3}>
        {/* An ask may be 8 questions of 8 long options. The drawer does not scroll and neither does the pane
            around it, so the questions scroll in a box of their own and Answer and the field stay in view. */}
        <div className={styles.questions} data-nc-ask-questions="">
          <VStack gap={3}>
            {ask.questions.map((question, index) => (
              <AskQuestionField
                /* The questions of one ask never change: the overlay re-sends the same list, in the same order. */
                key={index}
                question={question}
                draft={drafts[index]}
                onChange={(change) => edit(index, change)}
              />
            ))}
          </VStack>
        </div>
        <OperationFeedback feedback={feedback} />
        <HStack justify="end">
          <Button
            label="Answer"
            variant="primary"
            size="sm"
            isDisabled={answers === null}
            tooltip={answers === null ? 'Answer every question first.' : undefined}
            data-nc-ask-submit=""
            clickAction={async () => {
              if (answers === null) return;
              if (await feedback.run(onAnswer(ask.askId, answers), writeFailureText(ANSWER_ASK_FAILURES, ANSWER_ASK_TEXT))) {
                onSettled();
              }
            }}
          />
        </HStack>
      </VStack>
    </div>
  );
}

/**
 * One question in the Planner's words. Options are a radio list with the first, the recommended one,
 * picked; the reader's own words always fit beside them and, once written, are what is sent. A
 * question without options takes words only.
 */
function AskQuestionField({ question, draft, onChange }: {
  question: AskQuestion;
  draft: AskDraft;
  onChange: (change: Partial<AskDraft>) => void;
}) {
  if (question.options.length === 0) {
    return (
      <TextInput
        label={question.title}
        placeholder="Your answer"
        size="sm"
        value={draft.own}
        onChange={(own) => onChange({ own: clampAskAnswer(own) })}
      />
    );
  }
  return (
    <VStack gap={1}>
      <RadioList
        label={question.title}
        size="sm"
        value={String(draft.choice ?? 0)}
        onChange={(value) => onChange({ choice: Number(value) })}
      >
        {question.options.map((option, index) => (
          <RadioListItem
            /* Index, not text: two options may read the same, and the answer is the option's text either way. */
            key={index}
            value={String(index)}
            label={option}
            endContent={index === 0 ? <Badge variant="neutral" label="Recommended" /> : undefined}
          />
        ))}
      </RadioList>
      <TextInput
        label={`Your own answer: ${question.title}`}
        isLabelHidden
        placeholder="Or answer in your own words"
        size="sm"
        value={draft.own}
        onChange={(own) => onChange({ own: clampAskAnswer(own) })}
      />
    </VStack>
  );
}
