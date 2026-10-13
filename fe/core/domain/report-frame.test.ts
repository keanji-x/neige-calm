import { describe, expect, it } from 'vitest';

import { reportFrameHeight } from './report-frame.js';

describe('reportFrameHeight', () => {
  it('takes the fallback for an absent or NaN height and clamps the rest to the kernel bounds', () => {
    expect(reportFrameHeight(undefined, 480)).toBe(480);
    expect(reportFrameHeight(null, 360)).toBe(360);
    expect(reportFrameHeight(Number.NaN, 480)).toBe(480);
    expect(reportFrameHeight(640, 480)).toBe(640);
    expect(reportFrameHeight(50, 480)).toBe(120);
    expect(reportFrameHeight(9000, 480)).toBe(2000);
  });
});
