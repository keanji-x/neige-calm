export type PositionalColorToken =
  | '--bg' | '--paper' | '--hairline' | '--hairline-strong'
  | '--text' | '--text-2' | '--text-3' | '--text-4'
  | '--accent' | '--accent-soft' | '--warn' | '--warn-soft';

export type SurfaceToken =
  | '--surface-rail' | '--surface-card' | '--surface-chip' | '--surface-chip-focus' | '--surface-toggle-overlay'
  | '--surface-panel-head' | '--surface-terminal' | '--surface-code'
  | '--surface-paper' | '--surface-bg' | '--surface-hover-overlay';

export type OverlayToken =
  | '--overlay-hover-faint' | '--overlay-hover' | '--overlay-hover-strong' | '--overlay-active';

export type SemanticColorToken =
  | '--text-label' | '--text-meta' | '--text-decorative'
  | '--success' | '--error' | '--overlay-scrim'
  | '--cal-event-waiting-bg' | '--error-text' | '--warn-border'
  | '--warn-text' | '--success-text' | '--error-soft' | '--error-border'
  | '--text-on-accent';

/** Cove identity slots. A cove's colour is a slot name, never a free-form hex (§6.2). */
export type CoveIdentityToken =
  | '--cove-1' | '--cove-2' | '--cove-3' | '--cove-4'
  | '--cove-5' | '--cove-6' | '--cove-7' | '--cove-8';

export type ColorToken =
  | PositionalColorToken | SurfaceToken | OverlayToken | SemanticColorToken | CoveIdentityToken;

export type TypeScaleToken =
  | '--text-xs' | '--text-base' | '--text-md'
  | '--text-lg' | '--text-xl';

export type LeadingToken =
  | '--leading-none' | '--leading-tight' | '--leading-snug' | '--leading-base' | '--leading-loose';

export type TrackingToken =
  | '--tracking-tighter' | '--tracking-tight' | '--tracking-normal'
  | '--tracking-wide' | '--tracking-wider' | '--tracking-widest';

export type SpacingToken =
  | '--space-0' | '--space-px' | '--space-1' | '--space-2' | '--space-3' | '--space-4'
  | '--space-5' | '--space-6' | '--space-7' | '--space-8' | '--space-9' | '--space-10'
  | '--space-11' | '--space-12';

export type RadiusToken =
  | '--radius-sm' | '--radius-md' | '--radius-lg' | '--radius-pill';

export type MotionToken =
  | '--motion-instant' | '--motion-quick' | '--motion-snappy'
  | '--motion-medium' | '--motion-slow' | '--motion-pulse';

export type WeightToken =
  | '--weight-normal' | '--weight-medium' | '--weight-semibold';

export type ScalarToken =
  | TypeScaleToken | LeadingToken | TrackingToken | RadiusToken | SpacingToken | MotionToken
  | WeightToken;

/**
 * Box scale is its own top-level family, not part of ScalarToken: §9.1's managed-property
 * table takes its allowed values per family, and folding these in would make
 * `font-size: var(--rail-w)` a legal declaration.
 */
export type BoxScaleToken =
  | '--row-h-sm' | '--row-h' | '--row-h-lg'
  | '--control-h-sm' | '--control-h' | '--control-h-lg'
  | '--rail-w' | '--rail-w-collapsed' | '--panel-w' | '--drawer-w' | '--header-baseline'
  | '--measure-prose' | '--measure-form' | '--measure-page' | '--measure-board'
  | '--measure-list' | '--measure-doc'
  | '--slot-h' | '--rule-h' | '--dot-sm' | '--dot-md'
  | '--glyph-sm' | '--glyph' | '--menu-w-min' | '--menu-w-max';

/** A composite box-shadow: neither a colour nor a scalar. */
export type ShadowToken = '--shadow-float';

export type FontToken =
  | '--font-sans' | '--font-serif' | '--font-mono'
  | '--font-display' | '--font-numeric' | '--font-code';

export type ZIndexToken =
  | '--z-base' | '--z-raised' | '--z-sticky' | '--z-overlay' | '--z-modal' | '--z-toast';

export type StyleToken =
  | ColorToken | ScalarToken | FontToken | ZIndexToken | BoxScaleToken | ShadowToken;
