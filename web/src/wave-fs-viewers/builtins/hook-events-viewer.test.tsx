import { render, screen, within } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { HookEventsViewer } from './hook-events-viewer';

const Component = HookEventsViewer.Component;

afterEach(() => {
  vi.restoreAllMocks();
});

describe('HookEventsViewer', () => {
  it('renders hook events sorted by creation time with payload disclosures', () => {
    vi.spyOn(Date, 'now').mockReturnValue(
      new Date('2026-06-10T12:00:00Z').getTime(),
    );

    render(
      <Component
        path="cards/card_1/events.json"
        raw="[]"
        data={[
          {
            created_at: new Date('2026-06-10T11:30:00Z').getTime(),
            event_id: 2,
            kind: 'claude.hook',
            hook_kind: 'PostToolUse',
            payload: { tool: 'Read', ok: true },
          },
          {
            created_at: new Date('2026-06-10T11:00:00Z').getTime(),
            event_id: 1,
            kind: 'codex.hook',
            hook_kind: 'PreToolUse',
            payload: { tool: 'Bash' },
          },
        ]}
      />,
    );

    expect(
      screen.getByRole('heading', { name: 'Hook events (2)' }),
    ).toBeInTheDocument();

    const rows = screen.getAllByRole('listitem');
    expect(within(rows[0]).getByText('PreToolUse')).toHaveClass(
      'wave-fs-viewer-primary',
    );
    expect(within(rows[1]).getByText('PostToolUse')).toHaveClass(
      'wave-fs-viewer-primary',
    );
    expect(screen.getByText('codex.hook')).toHaveAttribute(
      'data-tone',
      'accent',
    );
    expect(screen.getByText('claude.hook')).toHaveAttribute(
      'data-tone',
      'warning',
    );
    expect(screen.getByText('Created 1h ago')).toBeInTheDocument();
    expect(screen.getByText('Created 30m ago')).toBeInTheDocument();

    const details = screen.getAllByText('Payload')[0].closest('details');
    expect(details).not.toHaveAttribute('open');
    expect(details?.querySelector('code')?.textContent).toBe(
      JSON.stringify({ tool: 'Bash' }, null, 2),
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
