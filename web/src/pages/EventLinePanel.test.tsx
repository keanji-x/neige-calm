import { render, screen, within } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { EventLinePanel } from './EventLinePanel';
import {
  isRuntimeLiveState,
  type EventLineEntry,
} from './useEventLineEntries';

function entry(overrides: Partial<EventLineEntry> = {}): EventLineEntry {
  return {
    id: 'entry_1',
    waveId: 'wave_1',
    identityKey: 'task:task.completed:task_1',
    time: new Date('2026-06-10T12:00:00Z').getTime(),
    tone: 'default',
    title: 'Task completed',
    tag: 'task',
    description: 'Worker task <code>task_1</code> completed.',
    ...overrides,
  };
}

describe('EventLinePanel', () => {
  afterEach(() => {
    vi.useRealTimers();
  });

  it('renders entries with titles and tag chips', () => {
    render(
      <EventLinePanel
        live={false}
        entries={[
          entry({ id: 'entry_1', title: 'Report regenerated', tag: 'agent' }),
          entry({ id: 'entry_2', title: 'Task failed', tag: 'alert' }),
        ]}
      />,
    );

    expect(screen.getByText('Report regenerated')).toBeInTheDocument();
    expect(screen.getByText('Task failed')).toBeInTheDocument();
    expect(screen.getByText('AGENT')).toBeInTheDocument();
    expect(screen.getByText('ALERT')).toBeInTheDocument();
  });

  it('renders an empty state when there are no entries', () => {
    render(<EventLinePanel live={false} entries={[]} />);

    expect(screen.getByText('Nothing yet.')).toBeInTheDocument();
  });

  it('marks the LIVE indicator when a runtime is live', () => {
    render(
      <EventLinePanel
        live={isRuntimeLiveState('Working')}
        entries={[entry()]}
      />,
    );

    const live = screen.getByLabelText('Live runtime');
    expect(live).toHaveAttribute('data-live', 'true');
    expect(within(live).getByText('LIVE')).toBeInTheDocument();
    expect(live.querySelector('.report-live-dot')).toBeTruthy();
  });

  it('does not mark the LIVE indicator for an idle runtime', () => {
    render(
      <EventLinePanel
        live={isRuntimeLiveState('Idle')}
        entries={[entry()]}
      />,
    );

    expect(screen.getByLabelText('No live runtime')).toHaveAttribute(
      'data-live',
      'false',
    );
  });

  it('applies the amber tone class', () => {
    const { container } = render(
      <EventLinePanel
        live={false}
        entries={[entry({ tone: 'amber', title: 'Task failed', tag: 'alert' })]}
      />,
    );

    expect(container.querySelector('.report-event--amber')).toBeTruthy();
  });

  it('shows the aggregation count when count is greater than one', () => {
    render(<EventLinePanel live={false} entries={[entry({ count: 3 })]} />);

    expect(screen.getByText('× 3')).toBeInTheDocument();
  });

  it('renders code segments in descriptions as code elements', () => {
    const { container } = render(
      <EventLinePanel live={false} entries={[entry()]} />,
    );

    expect(container.querySelector('.report-event-desc code')).toHaveTextContent(
      'task_1',
    );
  });

  it('keeps sub-minute timestamps as just now', () => {
    const now = new Date('2026-06-10T12:00:50Z');
    vi.useFakeTimers();
    vi.setSystemTime(now);

    render(
      <EventLinePanel
        live={false}
        entries={[entry({ time: now.getTime() - 50_000 })]}
      />,
    );

    expect(screen.getByText('just now')).toBeInTheDocument();
  });
});
