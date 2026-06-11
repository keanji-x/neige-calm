import { render, screen } from '@testing-library/react';
import { describe, expect, it } from 'vitest';
import { CardsIndexViewer } from './cards-index-viewer';

const Component = CardsIndexViewer.Component;

describe('CardsIndexViewer', () => {
  it('renders an empty cards header', () => {
    render(<Component data={[]} path="cards/index.json" />);

    expect(
      screen.getByRole('heading', { name: 'Cards in this wave (0)' }),
    ).toBeInTheDocument();
    expect(screen.queryAllByRole('listitem')).toHaveLength(0);
  });

  it('renders card chips, titles, ids, and sort values', () => {
    render(
      <Component
        path="cards/index.json"
        data={[
          {
            id: 'card_codex_1',
            kind: 'codex',
            title: 'Draft findings',
            sort: 10,
          },
          {
            id: 'card_report_1',
            kind: 'wave-report',
            title: 'Final report',
            sort: 20,
          },
        ]}
      />,
    );

    expect(
      screen.getByRole('heading', { name: 'Cards in this wave (2)' }),
    ).toBeInTheDocument();
    expect(screen.getByText('codex')).toHaveClass('wave-fs-viewer-kind');
    expect(screen.getByText('wave-report')).toHaveClass('wave-fs-viewer-kind');
    expect(screen.getByText('Draft findings')).toBeInTheDocument();
    expect(screen.getByText('Final report')).toBeInTheDocument();
    expect(screen.getByText('card_codex_1')).toBeInTheDocument();
    expect(screen.getByText('card_report_1')).toBeInTheDocument();
    expect(screen.getByText('sort 10')).toBeInTheDocument();
    expect(screen.getByText('sort 20')).toBeInTheDocument();
  });

  it('throws on non-array payloads', () => {
    expect(() => CardsIndexViewer.parse('{"id":"card_1"}')).toThrow(
      /must be an array/,
    );
  });
});
