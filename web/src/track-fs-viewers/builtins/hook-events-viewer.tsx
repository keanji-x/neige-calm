import type { TrackFsHookEvent } from '../../api/generated-events';
import { formatRelativeTime } from '../../shared/relativeTime';
import { ViewerChip, type ViewerChipTone } from '../chips';
import type { TrackFsViewer } from '../registry';
import { trackFsHookEventsSchema } from '../schemas';

export const HookEventsViewer: TrackFsViewer<TrackFsHookEvent[]> = {
  id: 'hook-events',
  match: (path) => /^cards\/[^/]+\/events\.json$/.test(path),
  parse: (raw) => trackFsHookEventsSchema.parse(JSON.parse(raw)),
  Component: HookEventsViewerComponent,
};

function HookEventsViewerComponent({
  data,
}: {
  data: TrackFsHookEvent[];
  path: string;
  raw: string;
}) {
  return (
    <section className="track-fs-viewer-info-card">
      <h2 className="track-fs-viewer-title">Hook events ({data.length})</h2>
      {data.length === 0 ? (
        <p className="track-fs-viewer-empty">No hook events yet.</p>
      ) : (
        <ul className="track-fs-viewer-list">
          {/* Backend emits event-log order (ORDER BY id ASC); do not re-sort. */}
          {data.map((event) => (
            <li className="track-fs-viewer-row" key={event.event_id}>
              <div className="track-fs-viewer-main">
                <span className="track-fs-viewer-primary">
                  {event.hook_kind}
                </span>
                <details className="track-fs-viewer-payload">
                  <summary>Payload</summary>
                  <pre className="track-fs-viewer-payload-pre">
                    <code>{formatPayload(event.payload)}</code>
                  </pre>
                </details>
              </div>
              <div className="track-fs-viewer-meta">
                <ViewerChip label={event.kind} tone={hookEventTone(event.kind)} />
                <span className="track-fs-viewer-small">
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
