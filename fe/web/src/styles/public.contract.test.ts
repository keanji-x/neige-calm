import { describe, expectTypeOf, it } from 'vitest';

// @ts-expect-error -- radius tokens are intentionally omitted in phase 1 P3.
import type { RadiusToken } from './public.js';
// @ts-expect-error -- the scrim's rgba exception is deliberately not a frozen public shape.
import type { ScrimShapeContract } from './public.js';
// @ts-expect-error -- GATE-TOKENS-030 remains an intentional soft-detector omission.
import type { FontSizeDriftScanner } from './public.js';
// @ts-expect-error -- GATE-TOKENS-031 remains an intentional soft-detector omission.
import type { LineHeightDriftScanner } from './public.js';
// @ts-expect-error -- GATE-TOKENS-032 remains an intentional soft-detector omission.
import type { LetterSpacingDriftScanner } from './public.js';
// @ts-expect-error -- GATE-TOKENS-033 remains an intentional soft-detector omission.
import type { RadiusDriftScanner } from './public.js';
// @ts-expect-error -- GATE-TOKENS-034 remains an intentional soft-detector omission.
import type { SpacingDriftScanner } from './public.js';
// @ts-expect-error -- GATE-TOKENS-035 remains an intentional soft-detector omission.
import type { TransitionDriftScanner } from './public.js';
// @ts-expect-error -- GATE-TOKENS-036 remains an intentional soft-detector omission.
import type { OrphanTokenScanner } from './public.js';
import type { ColorToken, ScalarToken, StyleToken, ZIndexToken } from './public.js';

describe('styles/tokens public type contract', () => {
  void (null as unknown as RadiusToken);
  void (null as unknown as ScrimShapeContract);
  void (null as unknown as FontSizeDriftScanner);
  void (null as unknown as LineHeightDriftScanner);
  void (null as unknown as LetterSpacingDriftScanner);
  void (null as unknown as RadiusDriftScanner);
  void (null as unknown as SpacingDriftScanner);
  void (null as unknown as TransitionDriftScanner);
  void (null as unknown as OrphanTokenScanner);

  it('exposes closed token-name families', () => {
    expectTypeOf<'--bg'>().toMatchTypeOf<ColorToken>();
    expectTypeOf<'--motion-pulse'>().toMatchTypeOf<ScalarToken>();
    expectTypeOf<'--z-toast'>().toMatchTypeOf<ZIndexToken>();
    expectTypeOf<ColorToken>().toMatchTypeOf<StyleToken>();
    expectTypeOf<ScalarToken>().toMatchTypeOf<StyleToken>();
    expectTypeOf<ZIndexToken>().toMatchTypeOf<StyleToken>();
  });

  it('rejects names outside the frozen public surface', () => {
    // @ts-expect-error -- radius is a documented intentional omission.
    const radius: StyleToken = '--radius-sm';
    // @ts-expect-error -- arbitrary custom properties are not frozen tokens.
    const unknown: StyleToken = '--new-token';
    void radius;
    void unknown;
  });
});
