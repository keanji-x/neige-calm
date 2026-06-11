import type { ReactNode } from 'react';
import type { EventLineEntry } from './useEventLineEntries';

export interface EventLinePanelProps {
  entries: EventLineEntry[];
  /** True when at least one runtime is live (lights up the LIVE indicator). */
  live: boolean;
}

function formatRelativeTime(time: number, now = Date.now()): string {
  if (!Number.isFinite(time) || time <= 0) return 'just now';

  const diffMs = Math.max(0, now - time);
  const seconds = Math.floor(diffMs / 1000);
  if (seconds < 45) return 'just now';

  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes}m ago`;

  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours}h ago`;

  const days = Math.floor(hours / 24);
  if (days < 30) return `${days}d ago`;

  return new Intl.DateTimeFormat(undefined, {
    month: 'short',
    day: 'numeric',
  }).format(new Date(time));
}

function renderDescription(description: string): ReactNode {
  const nodes: ReactNode[] = [];
  const codePattern = /<code>(.*?)<\/code>/g;
  let lastIndex = 0;
  let match: RegExpExecArray | null;

  while ((match = codePattern.exec(description)) !== null) {
    if (match.index > lastIndex) {
      nodes.push(description.slice(lastIndex, match.index));
    }
    nodes.push(<code key={`${match.index}:${match[1]}`}>{match[1]}</code>);
    lastIndex = match.index + match[0].length;
  }

  if (lastIndex < description.length) {
    nodes.push(description.slice(lastIndex));
  }

  return nodes.length > 0 ? nodes : description;
}

export function EventLinePanel({ entries, live }: EventLinePanelProps) {
  return (
    <div className="report-event-panel">
      <header className="report-rail-head report-event-head">
        <span>Event line</span>
        <span
          className="report-live"
          data-live={live ? 'true' : 'false'}
          aria-label={live ? 'Live runtime' : 'No live runtime'}
        >
          <span className="report-live-dot" aria-hidden="true" />
          LIVE
        </span>
      </header>

      {entries.length === 0 ? (
        <div className="report-event-empty">
          Nothing yet.
        </div>
      ) : (
        <div className="report-event-line" role="list">
          {entries.map((entry) => (
            <div
              key={entry.id}
              className={`report-event report-event--${entry.tone}`}
              role="listitem"
            >
              <span className="report-event-dot" aria-hidden="true" />
              <div className="report-event-body">
                <div className="report-event-meta">
                  <time
                    className="report-event-time"
                    dateTime={new Date(entry.time).toISOString()}
                  >
                    {formatRelativeTime(entry.time)}
                  </time>
                  <span className="report-event-tag">
                    {entry.tag.toUpperCase()}
                  </span>
                </div>
                <div className="report-event-title">
                  <span>{entry.title}</span>
                  {entry.count && entry.count > 1 ? (
                    <span className="report-event-count">× {entry.count}</span>
                  ) : null}
                </div>
                <div className="report-event-desc">
                  {renderDescription(entry.description)}
                </div>
              </div>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}
