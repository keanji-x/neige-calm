// @vitest-environment jsdom
import { cleanup, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it } from 'vitest';

import { ReportAppBlock } from './public.tsx';

afterEach(cleanup);

describe('ReportAppBlock', () => {
  it('sandboxes the frame without same-origin, so it cannot reach this document', () => {
    const { container } = render(<ReportAppBlock payload={{ src: '/apps/timeline.html' }} />);
    const sandbox = container.querySelector('iframe')?.getAttribute('sandbox') ?? '';
    expect(sandbox).toContain('allow-scripts');
    // The pair `allow-scripts allow-same-origin` lets the frame remove its own
    // sandbox attribute, which is the same as never having one.
    expect(sandbox).not.toContain('allow-same-origin');
  });

  it('falls back to the src when the payload names no title', () => {
    render(<ReportAppBlock payload={{ src: '/apps/timeline.html' }} />);
    expect(screen.getByTitle('/apps/timeline.html')).toBeTruthy();
  });

  it('clamps an out-of-range height rather than refusing the block', () => {
    const { container } = render(<ReportAppBlock payload={{ src: '/a', height: 9000 }} />);
    expect(container.querySelector('iframe')?.style.blockSize).toBe('2000px');
  });

  // The schema already refuses a foreign origin; this is the second check, on
  // the resolved URL, because a regex and a URL parser disagree about exactly
  // the inputs worth attacking.
  it('loads nothing when the resolved src leaves this origin', () => {
    // `/\evil.example/x` is also what the schema catches, and it is here as
    // well because the browser turns the backslash into a slash *while
    // resolving*: a prefix check on the raw string sees `/…`, the resolver sees
    // `//evil.example`.
    const { container } = render(<ReportAppBlock payload={{ src: '/\\evil.example/x' }} />);
    expect(container.querySelector('iframe')).toBeNull();
    expect(container.textContent).toContain('not a same-origin path');
  });
});
