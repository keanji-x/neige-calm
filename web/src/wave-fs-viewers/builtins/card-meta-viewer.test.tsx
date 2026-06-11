import { render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { CardMetaViewer } from './card-meta-viewer';

const Component = CardMetaViewer.Component;

afterEach(() => {
  vi.restoreAllMocks();
});

describe('CardMetaViewer', () => {
  it('renders kind, id, role, sort, timestamps, and deletable state', () => {
    vi.spyOn(Date, 'now').mockReturnValue(
      new Date('2026-06-10T12:00:00Z').getTime(),
    );

    render(
      <Component
        path="cards/card_1/meta.json"
        raw="{}"
        data={{
          id: 'card_1',
          kind: 'codex',
          role: 'worker',
          sort: 10,
          deletable: false,
          createdAt: new Date('2026-06-10T10:00:00Z').getTime(),
          updatedAt: new Date('2026-06-10T11:55:00Z').getTime(),
        }}
      />,
    );

    expect(screen.getByRole('heading', { name: 'codex' })).toHaveClass(
      'wave-fs-viewer-primary',
    );
    expect(screen.getByText('card_1')).toHaveClass('wave-fs-viewer-mono');
    expect(screen.getByText('worker')).toHaveClass('wave-fs-viewer-chip');
    expect(screen.getByText('sort 10')).toBeInTheDocument();
    expect(screen.getByText('Created 2h ago')).toBeInTheDocument();
    expect(screen.getByText('Updated 5m ago')).toBeInTheDocument();
    expect(screen.getByText('deletable: no')).toBeInTheDocument();
  });

  it('renders placeholders with all optional fields missing', () => {
    const { container } = render(
      <Component
        path="cards/card_1/meta.json"
        raw="{}"
        data={{ id: 'card_1', kind: 'terminal' }}
      />,
    );

    expect(screen.getByRole('heading', { name: 'terminal' })).toBeTruthy();
    expect(screen.getByText('sort -')).toBeInTheDocument();
    expect(screen.getByText('Created -')).toBeInTheDocument();
    expect(screen.getByText('Updated -')).toBeInTheDocument();
    expect(screen.getByText('deletable: -')).toBeInTheDocument();
    expect(container.querySelector('.wave-fs-viewer-chip')).toBeNull();
  });

  it('throws when required fields are missing', () => {
    expect(() => CardMetaViewer.parse('{"kind":"codex"}')).toThrow(
      /id string/,
    );
    expect(() => CardMetaViewer.parse('[]')).toThrow(/must be an object/);
  });
});
