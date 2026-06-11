import { afterEach, describe, expect, it, vi } from 'vitest';
import { formatRelativeTime, formatUpdatedAt } from './relativeTime';

afterEach(() => {
  vi.restoreAllMocks();
});

describe('relative time helpers', () => {
  it('keeps the existing updated-at wording', () => {
    vi.spyOn(Date, 'now').mockReturnValue(
      new Date('2026-06-10T12:00:00Z').getTime(),
    );

    expect(
      formatUpdatedAt(new Date('2026-06-10T10:00:00Z').getTime()),
    ).toBe('Updated 2h ago');
  });

  it('formats custom labels and invalid placeholders', () => {
    vi.spyOn(Date, 'now').mockReturnValue(
      new Date('2026-06-10T12:00:00Z').getTime(),
    );

    expect(
      formatRelativeTime('Created', new Date('2026-06-10T11:55:00Z').getTime()),
    ).toBe('Created 5m ago');
    expect(formatRelativeTime('Finished', null)).toBe('Finished -');
  });
});
