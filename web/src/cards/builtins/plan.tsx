import { z } from 'zod';
import type { PlanCardData } from '../../types';
import type { CardEntry } from '../registry';

/**
 * Wire shape for a `kind: "plan"` card's `payload`. Plan cards are seed-
 * only today; the schema is defined here so when Slice G adds creation
 * the wire→UI hop already has a parse boundary.
 */
const planStepSchema = z.object({
  label: z.string(),
  done: z.boolean().optional(),
  cur: z.boolean().optional(),
  when: z.string().optional(),
});

const planPayloadSchema = z.object({
  steps: z.array(planStepSchema),
});

function PlanCard({ card }: { card: PlanCardData }) {
  return (
    <div className="plan-card">
      <div className="plan-card-head card-drag-handle">Plan</div>
      <ol className="plan-card-body">
        {card.steps.map((s, i) => (
          <li key={i} className={s.done ? 'done' : s.cur ? 'cur' : ''}>
            <span className="dot">{s.done && '✓'}</span>
            <span className="lbl">{s.label}</span>
            {s.when && <span className="when">{s.when}</span>}
          </li>
        ))}
      </ol>
    </div>
  );
}

export const PlanEntry: CardEntry<PlanCardData> = {
  type: 'plan',
  Component: PlanCard,
  defaultSize: { w: 4, h: 7, minW: 3, minH: 4 },
  // Plan cards are seed-only today. Slice G's AddPanel will use this
  // metadata to advertise the menu entry; CalmApp.addCard wires the
  // actual creation in that slice. `fromKernel` is scoped to
  // `kind === 'plan'` so it stays a no-op until then.
  fromKernel: (k) => {
    if (k.kind !== 'plan') return null;
    const parsed = planPayloadSchema.safeParse(k.payload);
    if (!parsed.success) {
      // eslint-disable-next-line no-console
      console.warn(
        `[cards] plan payload invalid for ${k.id}:`,
        parsed.error.issues,
      );
      return null;
    }
    return {
      type: 'plan',
      id: k.id,
      steps: parsed.data.steps,
    };
  },
  addPanel: { label: 'New plan', icon: 'plan' },
};
