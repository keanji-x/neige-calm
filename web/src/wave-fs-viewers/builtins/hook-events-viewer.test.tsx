import { render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { HookEventsViewer } from './hook-events-viewer';

const Component = HookEventsViewer.Component;

afterEach(() => {
  vi.restoreAllMocks();
});

describe('HookEventsViewer', () => {
  it('renders hook events in backend event-log order with payload disclosures', () => {
    const base = new Date('2026-06-10T11:00:00Z').getTime();
    vi.spyOn(Date, 'now').mockReturnValue(base + 3_600_000 + 1_000);

    render(
      <Component
        path="cards/card_1/events.json"
        raw="[]"
        data={[
          {
            created_at: base + 300,
            event_id: 1,
            kind: 'claude.hook',
            hook_kind: 'PostToolUse',
            payload: { tool: 'Read', ok: true },
          },
          {
            created_at: base + 100,
            event_id: 2,
            kind: 'codex.hook',
            hook_kind: 'PreToolUse',
            payload: { tool: 'Bash' },
          },
          {
            created_at: base + 200,
            event_id: 3,
            kind: 'codex.hook',
            hook_kind: 'Stop',
            payload: { transcript: 'done' },
          },
        ]}
      />,
    );

    expect(
      screen.getByRole('heading', { name: 'Hook events (3)' }),
    ).toBeInTheDocument();

    const rows = screen.getAllByRole('listitem');
    expect(
      rows.map(
        (row) => row.querySelector('.wave-fs-viewer-primary')?.textContent,
      ),
    ).toEqual(['PostToolUse', 'PreToolUse', 'Stop']);
    expect(screen.getAllByText('codex.hook')[0]).toHaveAttribute(
      'data-tone',
      'accent',
    );
    expect(screen.getByText('claude.hook')).toHaveAttribute(
      'data-tone',
      'warning',
    );
    expect(screen.getAllByText('Created 1h ago')).toHaveLength(3);

    const details = screen.getAllByText('Payload')[0].closest('details');
    expect(details).not.toHaveAttribute('open');
    expect(details?.querySelector('code')?.textContent).toBe(
      JSON.stringify({ tool: 'Read', ok: true }, null, 2),
    );
  });

  it('renders the empty state', () => {
    render(<Component data={[]} path="cards/card_1/events.json" raw="[]" />);

    expect(
      screen.getByRole('heading', { name: 'Hook events (0)' }),
    ).toBeInTheDocument();
    expect(screen.getByText('No hook events yet.')).toHaveClass(
      'wave-fs-viewer-empty',
    );
  });

  it('uses a neutral chip tone for open hook event kinds', () => {
    render(
      <Component
        path="cards/card_1/events.json"
        raw="[]"
        data={[
          {
            created_at: 0,
            event_id: 1,
            kind: 'custom.hook',
            hook_kind: 'Notify',
            payload: null,
          },
        ]}
      />,
    );

    expect(screen.getByText('custom.hook')).toHaveAttribute(
      'data-tone',
      'neutral',
    );
  });

  it('throws on non-array payloads without logging', () => {
    const consoleError = vi.spyOn(console, 'error').mockImplementation(() => {});

    expect(() => HookEventsViewer.parse('{"event_id":1}')).toThrow();
    expect(consoleError).not.toHaveBeenCalled();
  });
});
