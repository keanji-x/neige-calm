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
  '--bg', '--paper', '--hairline', '--hairline-strong', '--text', '--text-2', '--text-3',
  '--text-4', '--accent', '--accent-soft', '--warn', '--warn-soft', '--surface-rail',
  '--surface-card', '--surface-chip', '--surface-chip-focus', '--surface-panel-head',
  '--surface-terminal', '--surface-code', '--overlay-hover-faint', '--overlay-hover',
  '--overlay-hover-strong', '--overlay-active', '--success', '--error', '--overlay-scrim',
  '--error-text', '--warn-border', '--text-xs', '--text-base', '--text-md',
  '--text-lg', '--text-xl', '--leading-none',
  '--leading-tight', '--leading-snug', '--leading-base', '--leading-loose', '--tracking-tighter',
  '--tracking-tight', '--tracking-normal', '--tracking-wide', '--tracking-wider',
  '--tracking-widest', '--radius-sm', '--radius-md', '--radius-lg',
  '--radius-pill', '--space-0', '--space-px', '--space-1', '--space-2', '--space-3', '--space-4',
  '--space-5', '--space-6', '--space-7', '--space-8', '--space-9', '--space-10', '--space-11',
  '--space-12', '--motion-instant', '--motion-quick', '--motion-snappy', '--motion-medium',
  '--motion-slow', '--motion-pulse', '--font-sans', '--font-serif', '--font-mono',
  '--font-display', '--font-numeric', '--font-code', '--z-base', '--z-raised', '--z-sticky',
  '--z-overlay', '--z-modal', '--z-toast',
  '--weight-normal', '--weight-medium', '--weight-semibold',
  '--row-h-sm', '--row-h', '--row-h-lg',
  '--control-h-sm', '--control-h', '--control-h-lg',
  '--rail-w', '--rail-w-collapsed', '--panel-w', '--drawer-w', '--header-baseline',
  '--measure-prose', '--measure-form', '--measure-page', '--measure-board',
  '--measure-list', '--measure-doc',
  '--slot-h', '--rule-h', '--dot-sm', '--dot-md',
  '--glyph-sm', '--glyph', '--menu-w-min', '--menu-w-max',
  '--warn-text', '--success-text', '--error-soft', '--error-border', '--text-on-accent',
  '--shadow-float',
  '--cove-1', '--cove-2', '--cove-3', '--cove-4',
  '--cove-5', '--cove-6', '--cove-7', '--cove-8',
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
