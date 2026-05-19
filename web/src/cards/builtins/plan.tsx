import type { PlanCardData } from '../../types';
import type { CardEntry } from '../registry';

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
  // No `fromKernel`: plan cards are seed-only today. Slice G's AddPanel
  // will use this metadata to advertise the menu entry; CalmApp.addCard
  // wires the actual creation in that slice.
  addPanel: { label: 'New plan', icon: 'plan' },
};
