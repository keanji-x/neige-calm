import type { WaveFsHookEvent } from '../../api/generated-events';
import { formatRelativeTime } from '../../shared/relativeTime';
import { ViewerChip, type ViewerChipTone } from '../chips';
import type { WaveFsViewer } from '../registry';
import { waveFsHookEventsSchema } from '../schemas';

export const HookEventsViewer: WaveFsViewer<WaveFsHookEvent[]> = {
  id: 'hook-events',
  match: (path) => /^cards\/[^/]+\/events\.json$/.test(path),
  parse: (raw) => waveFsHookEventsSchema.parse(JSON.parse(raw)),
  Component: HookEventsViewerComponent,
};

function HookEventsViewerComponent({
  data,
}: {
  data: WaveFsHookEvent[];
  path: string;
  raw: string;
}) {
  const events = [...data].sort(
    (a, b) => a.created_at - b.created_at || a.event_id - b.event_id,
  );

  return (
    <section className="wave-fs-viewer-info-card">
      <h2 className="wave-fs-viewer-title">Hook events ({data.length})</h2>
      {events.length === 0 ? (
        <p className="wave-fs-viewer-empty">No hook events yet.</p>
      ) : (
        <ul className="wave-fs-viewer-list">
          {events.map((event) => (
            <li className="wave-fs-viewer-row" key={event.event_id}>
              <div className="wave-fs-viewer-main">
                <span className="wave-fs-viewer-primary">
                  {event.hook_kind}
                </span>
                <details className="wave-fs-viewer-payload">
                  <summary>Payload</summary>
                  <pre className="wave-fs-viewer-payload-pre">
                    <code>{formatPayload(event.payload)}</code>
                  </pre>
                </details>
              </div>
              <div className="wave-fs-viewer-meta">
                <ViewerChip label={event.kind} tone={hookEventTone(event.kind)} />
                <span className="wave-fs-viewer-small">
                  {formatRelativeTime('Created', event.created_at)}
                </span>
              </div>
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}

function hookEventTone(kind: string): ViewerChipTone {
  switch (kind) {
    case 'codex.hook':
      return 'accent';
    case 'claude.hook':
      return 'warning';
    default:
      return 'neutral';
  }
}

function formatPayload(payload: unknown): string {
  return JSON.stringify(payload, null, 2) ?? String(payload);
}
