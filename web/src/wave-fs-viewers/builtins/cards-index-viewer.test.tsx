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
    expect(screen.getByText('No cards in this wave.')).toBeInTheDocument();
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
            deletable: true,
            created_at: 100,
            updated_at: 200,
          },
          {
            id: 'card_report_1',
            kind: 'wave-report',
            role: 'reportcard',
            sort: 20,
            deletable: false,
            created_at: 300,
            updated_at: 400,
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
    expect(screen.getByText('worker')).toHaveClass('wave-fs-viewer-chip');
    expect(screen.getByText('sort 10')).toBeInTheDocument();
    expect(screen.getByText('sort 20')).toBeInTheDocument();
  });

  it('rejects entries missing generated fields without warnings', () => {
    const consoleError = vi.spyOn(console, 'error').mockImplementation(() => {});

    expect(() =>
      CardsIndexViewer.parse(
        JSON.stringify([
          {
            id: 'card_1',
            kind: 'spec',
            role: null,
            sort: 0,
            deletable: true,
            created_at: 0,
            updated_at: 0,
          },
        ]),
      ),
    ).toThrow();
    expect(consoleError).not.toHaveBeenCalled();
  });

  it('throws on non-array payloads and entries missing kind', () => {
    expect(() => CardsIndexViewer.parse('{"id":"card_1"}')).toThrow();
    expect(() => CardsIndexViewer.parse('[{"id":"card_1"}]')).toThrow();
  });
});
