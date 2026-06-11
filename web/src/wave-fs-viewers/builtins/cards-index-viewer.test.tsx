import { render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { CardsIndexViewer } from './cards-index-viewer';

const Component = CardsIndexViewer.Component;

afterEach(() => {
  vi.restoreAllMocks();
});

describe('CardsIndexViewer', () => {
  it('renders an empty cards header', () => {
    render(<Component data={[]} path="cards/index.json" raw="[]" />);

    expect(
      screen.getByRole('heading', { name: 'Cards in this wave (0)' }),
    ).toBeInTheDocument();
    expect(screen.queryAllByRole('listitem')).toHaveLength(0);
  });

  it('renders card kinds, ids, roles, and sort values', () => {
    render(
      <Component
        path="cards/index.json"
        raw="[]"
        data={[
          {
            id: 'card_codex_1',
            kind: 'codex',
            role: 'worker',
            sort: 10,
          },
          {
            id: 'card_report_1',
            kind: 'wave-report',
            sort: 20,
          },
        ]}
      />,
    );

    expect(
      screen.getByRole('heading', { name: 'Cards in this wave (2)' }),
    ).toBeInTheDocument();
    expect(screen.getByText('codex')).toHaveClass(
      'wave-fs-viewer-card-title',
    );
    expect(screen.getByText('wave-report')).toHaveClass(
      'wave-fs-viewer-card-title',
    );
    expect(screen.getByText('card_codex_1')).toBeInTheDocument();
    expect(screen.getByText('card_report_1')).toBeInTheDocument();
    expect(screen.getByText('worker')).toHaveClass(
      'wave-fs-viewer-card-role',
    );
    expect(screen.getByText('sort 10')).toBeInTheDocument();
    expect(screen.getByText('sort 20')).toBeInTheDocument();
  });

  it('renders optional field fallbacks without warnings', () => {
    const consoleError = vi.spyOn(console, 'error').mockImplementation(() => {});

    const { container } = render(
      <Component
        path="cards/index.json"
        raw="[]"
        data={[
          { kind: 'spec' },
          {
            id: 'x',
            kind: 'terminal',
          },
        ]}
      />,
    );

    expect(screen.getByText('spec')).toHaveClass(
      'wave-fs-viewer-card-title',
    );
    expect(screen.getByText('terminal')).toHaveClass(
      'wave-fs-viewer-card-title',
    );
    expect(screen.getByText('missing-id')).toHaveClass(
      'wave-fs-viewer-card-id',
    );
    expect(screen.getByText('x')).toHaveClass('wave-fs-viewer-card-id');
    expect(screen.getAllByText('sort -')).toHaveLength(2);
    expect(container.querySelector('.wave-fs-viewer-card-role')).toBeNull();
    expect(consoleError).not.toHaveBeenCalled();
  });

  it('throws on non-array payloads and entries missing kind', () => {
    expect(() => CardsIndexViewer.parse('{"id":"card_1"}')).toThrow(
      /must be an array/,
    );
    expect(() => CardsIndexViewer.parse('[{"id":"card_1"}]')).toThrow(
      /must include a kind string/,
    );
  });
});
