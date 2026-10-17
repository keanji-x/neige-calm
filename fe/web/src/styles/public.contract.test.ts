import { GENERATED_TOKEN_INVENTORY } from './typography.generated.ts';
import { describe, expectTypeOf, it } from 'vitest';

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

const TOKEN_INVENTORY = [
  '--accent',
  '--accent-soft',
  '--area-1',
  '--area-2',
  '--area-3',
  '--area-4',
  '--area-5',
  '--area-6',
  '--area-7',
  '--area-8',
  '--bg',
  '--chip-accent',
  '--chip-error',
  '--chip-neutral',
  '--chip-on-accent',
  '--chip-on-error',
  '--chip-on-success',
  '--chip-on-warning',
  '--chip-success',
  '--chip-text-primary',
  '--chip-warning',
  '--control-h',
  '--control-h-lg',
  '--control-h-sm',
  '--control-h-touch',
  '--dot-md',
  '--dot-sm',
  '--drawer-w',
  '--ease-emphasis',
  '--ease-enter',
  '--ease-exit',
  '--ease-feedback',
  '--ease-layout',
  '--error',
  '--error-border',
  '--error-soft',
  '--error-text',
  '--font-code',
  '--font-display',
  '--font-mono',
  '--font-numeric',
  '--font-sans',
  '--font-serif',
  '--glyph',
  '--glyph-display',
  '--glyph-lg',
  '--glyph-meta',
  '--glyph-sm',
  '--glyph-xl',
  '--hairline',
  '--hairline-strong',
  '--header-baseline',
  '--header-primary-h',
  '--measure-board',
  '--measure-doc',
  '--measure-form',
  '--measure-list',
  '--measure-page',
  '--measure-prose',
  '--menu-w-max',
  '--menu-w-min',
  '--motion-instant',
  '--motion-medium',
  '--motion-pulse',
  '--motion-quick',
  '--motion-slow',
  '--motion-snappy',
  '--overlay-active',
  '--overlay-hover',
  '--overlay-hover-faint',
  '--overlay-hover-strong',
  '--overlay-scrim',
  '--panel-w',
  '--paper',
  '--radius-lg',
  '--radius-md',
  '--radius-pill',
  '--radius-sm',
  '--rail-w',
  '--rail-w-collapsed',
  '--row-h',
  '--row-h-lg',
  '--row-h-sm',
  '--rule-h',
  '--shadow-float',
  '--slot-h',
  '--success',
  '--success-text',
  '--surface-card',
  '--surface-chip',
  '--surface-code',
  '--surface-navigation',
  '--surface-rail',
  '--surface-terminal',
  '--text',
  '--text-2',
  '--text-3',
  '--text-4',
  '--text-on-accent',
  '--warn',
  '--warn-border',
  '--warn-soft',
  '--warn-text',
  '--weight-bold',
  '--weight-medium',
  '--weight-normal',
  '--weight-semibold',
  '--z-base',
  '--z-modal',
  '--z-overlay',
  '--z-raised',
  '--z-sticky',
  '--z-toast',
  ...GENERATED_TOKEN_INVENTORY,
] as const;

describe('styles/tokens public type contract', () => {
  void TOKEN_INVENTORY;
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
    expectTypeOf<StyleToken>().toEqualTypeOf<(typeof TOKEN_INVENTORY)[number]>();
    expectTypeOf<'--bg'>().toMatchTypeOf<ColorToken>();
    expectTypeOf<'--motion-pulse'>().toMatchTypeOf<ScalarToken>();
    expectTypeOf<'--radius-pill'>().toMatchTypeOf<RadiusToken>();
    expectTypeOf<'--z-toast'>().toMatchTypeOf<ZIndexToken>();
    expectTypeOf<ColorToken>().toMatchTypeOf<StyleToken>();
    expectTypeOf<ScalarToken>().toMatchTypeOf<StyleToken>();
    expectTypeOf<ZIndexToken>().toMatchTypeOf<StyleToken>();
  });

  it('rejects names outside the frozen public surface', () => {
    // @ts-expect-error -- arbitrary custom properties are not frozen tokens.
    const unknown: StyleToken = '--new-token';
    void unknown;
  });
});
