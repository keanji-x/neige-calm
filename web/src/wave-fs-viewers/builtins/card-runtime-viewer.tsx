import type { CardRuntimeView } from '../../api/generated-events';
import { runtimeStatusTones, ViewerChip } from '../chips';
import type { WaveFsViewer } from '../registry';
import { cardRuntimeSchema } from '../schemas';

export const CardRuntimeViewer: WaveFsViewer<CardRuntimeView | null> = {
  id: 'card-runtime',
  match: (path) => /^cards\/[^/]+\/runtime\.json$/.test(path),
  parse: (raw) => cardRuntimeSchema.parse(JSON.parse(raw)),
  Component: CardRuntimeViewerComponent,
};

const runtimeFieldLabels = {
  terminal_id: 'terminal_id',
  thread_id: 'thread_id',
  session_id: 'session_id',
  source: 'source',
  thread_status: 'thread_status',
} satisfies Record<
  keyof Pick<
    CardRuntimeView,
    'terminal_id' | 'thread_id' | 'session_id' | 'source' | 'thread_status'
  >,
  string
>;

function CardRuntimeViewerComponent({
  data,
}: {
  data: CardRuntimeView | null;
  path: string;
  raw: string;
}) {
  if (data === null) {
    return (
      <section className="wave-fs-viewer-info-card">
        <p className="wave-fs-viewer-empty">No runtime attached.</p>
      </section>
    );
  }

  return (
    <section className="wave-fs-viewer-info-card">
      <h2 className="wave-fs-viewer-primary">{data.kind}</h2>
      <div className="wave-fs-viewer-row">
        <span className="wave-fs-viewer-mono">{data.runtime_id}</span>
        <ViewerChip label={data.status} tone={runtimeStatusTones[data.status]} />
        {data.provider ? <ViewerChip label={data.provider} /> : null}
      </div>
      {Object.entries(runtimeFieldLabels).map(([key, label]) => {
        const value = data[key as keyof typeof runtimeFieldLabels];
        if (!value) return null;
        return (
          <div className="wave-fs-viewer-field" key={key}>
            <span className="wave-fs-viewer-label">{label}</span>
            <span className="wave-fs-viewer-mono wave-fs-viewer-break">
              {value}
            </span>
          </div>
        );
      })}
    </section>
  );
}
