import type { ObjectiveBlockPayload } from '../../../../../core/domain/report-objective.ts';
import { InlineTable } from '../table/inline.tsx';

/** A definition is read-only; no execution state is inferred from its budget. */
export function ReportObjectiveBlock({ payload }: { payload: ObjectiveBlockPayload }) {
  return <InlineTable payload={{
    caption: payload.title,
    columns: [{ key: 'field', label: 'Objective' }, { key: 'value', label: 'Value' }],
    rows: [
      { field: 'Key', value: payload.key },
      { field: 'Budget turns', value: payload.budget.turns },
      { field: 'Budget tokens', value: payload.budget.tokens },
      { field: 'Budget wall time (seconds)', value: payload.budget.wall_secs },
    ],
  }} />;
}
