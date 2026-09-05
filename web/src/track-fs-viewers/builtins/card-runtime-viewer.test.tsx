import { render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { CardRuntimeViewer } from './card-runtime-viewer';

const Component = CardRuntimeViewer.Component;

afterEach(() => {
  vi.restoreAllMocks();
});

describe('CardRuntimeViewer', () => {
  it('renders runtime identity, status, provider, and optional fields', () => {
    render(
      <Component
        path="cards/card_1/runtime.json"
        raw="{}"
        data={{
          worker_session_id: 'runtime_1',
          kind: 'codex',
          status: 'running',
          provider: 'codex',
          terminal_id: 'term_1',
          thread_id: 'thread_1',
          session_id: 'session_1',
          source: 'track-dispatcher',
          thread_status: 'turn_running',
        }}
      />,
    );

    expect(screen.getByRole('heading', { name: 'codex' })).toHaveClass(
      'track-fs-viewer-primary',
    );
    expect(screen.getByText('runtime_1')).toHaveClass('track-fs-viewer-mono');
    expect(screen.getByText('running')).toHaveAttribute('data-tone', 'accent');
    expect(screen.getByText('codex', { selector: '.track-fs-viewer-chip' }))
      .toBeInTheDocument();
    expect(screen.getByText('terminal_id')).toHaveClass('track-fs-viewer-label');
    expect(screen.getByText('term_1')).toHaveClass('track-fs-viewer-mono');
    expect(screen.getByText('thread_id')).toHaveClass('track-fs-viewer-label');
    expect(screen.getByText('thread_1')).toHaveClass('track-fs-viewer-mono');
    expect(screen.getByText('session_id')).toHaveClass('track-fs-viewer-label');
    expect(screen.getByText('session_1')).toHaveClass('track-fs-viewer-mono');
    expect(screen.getByText('source')).toHaveClass('track-fs-viewer-label');
    expect(screen.getByText('track-dispatcher')).toHaveClass(
      'track-fs-viewer-mono',
    );
    expect(screen.getByText('thread_status')).toHaveClass(
      'track-fs-viewer-label',
    );
    expect(screen.getByText('turn_running')).toHaveClass('track-fs-viewer-mono');
  });

  it('renders the null runtime empty state', () => {
    render(
      <Component data={null} path="cards/card_1/runtime.json" raw="null" />,
    );

    expect(screen.getByText('No runtime attached.')).toHaveClass(
      'track-fs-viewer-empty',
    );
  });

  it('omits optional rows when fields are absent', () => {
    render(
      <Component
        path="cards/card_1/runtime.json"
        raw="{}"
        data={{
          worker_session_id: 'runtime_min',
          kind: 'terminal',
          status: 'idle',
        }}
      />,
    );

    expect(screen.getByRole('heading', { name: 'terminal' })).toBeTruthy();
    expect(screen.getByText('runtime_min')).toHaveClass('track-fs-viewer-mono');
    expect(screen.getByText('idle')).toHaveAttribute('data-tone', 'neutral');
    expect(screen.queryByText('provider')).toBeNull();
    expect(screen.queryByText('terminal_id')).toBeNull();
    expect(screen.queryByText('thread_id')).toBeNull();
    expect(screen.queryByText('session_id')).toBeNull();
    expect(screen.queryByText('source')).toBeNull();
    expect(screen.queryByText('thread_status')).toBeNull();
  });

  it('throws when required fields are missing without logging', () => {
    const consoleError = vi.spyOn(console, 'error').mockImplementation(() => {});

    expect(() => CardRuntimeViewer.parse('{"status":"running"}')).toThrow();
    expect(() =>
      CardRuntimeViewer.parse(
        JSON.stringify({
          worker_session_id: 'runtime_1',
          kind: 'codex',
          status: null,
        }),
      ),
    ).toThrow();
    expect(consoleError).not.toHaveBeenCalled();
  });
});
