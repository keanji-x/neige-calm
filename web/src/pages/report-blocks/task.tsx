import ReactMarkdown from 'react-markdown';
import remarkGfm from 'remark-gfm';
import type { TaskBlockPayload } from '../../cards/builtins/wave-report';
import { ReportLink, reportUrlTransform } from './index';

export function ReportTaskBlock({ payload }: { payload: TaskBlockPayload }) {
  if (payload.tombstone) {
    return (
      <section className="rb-task" aria-label={`Withdrawn task ${payload.key}`}>
        <strong>{payload.key}</strong>
        <p>Withdrawn{payload.tombstone.reason ? `: ${payload.tombstone.reason}` : ''}</p>
      </section>
    );
  }
  return (
    <section className="rb-task" aria-label={`Task ${payload.key}`}>
      <header>
        <strong>{payload.key}</strong>
        {payload.kind && <span>{payload.kind}</span>}
      </header>
      {payload.goal && (
        <div className="calm-prose rb-task-goal">
          <ReactMarkdown remarkPlugins={[remarkGfm]} urlTransform={reportUrlTransform}
            components={{ a: ReportLink }}>{payload.goal}</ReactMarkdown>
        </div>
      )}
      {payload.acceptance && (
        <div className="rb-task-acceptance">
          <span>Acceptance</span>
          <div className="calm-prose">
            <ReactMarkdown remarkPlugins={[remarkGfm]} urlTransform={reportUrlTransform}
              components={{ a: ReportLink }}>{payload.acceptance}</ReactMarkdown>
          </div>
        </div>
      )}
      {payload.depends_on && payload.depends_on.length > 0 && (
        <p>Depends on: {payload.depends_on.join(', ')}</p>
      )}
    </section>
  );
}
