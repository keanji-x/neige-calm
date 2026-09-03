export type PositionalColorToken =
  | '--bg' | '--paper' | '--hairline' | '--hairline-strong'
  | '--text' | '--text-2' | '--text-3' | '--text-4'
  | '--accent' | '--accent-soft' | '--warn' | '--warn-soft';

export type SurfaceToken =
  | '--surface-rail' | '--surface-card' | '--surface-chip'
  | '--surface-terminal' | '--surface-code';

export type OverlayToken =
  | '--overlay-hover-faint' | '--overlay-hover' | '--overlay-hover-strong' | '--overlay-active';

export type SemanticColorToken =
  | '--success' | '--error' | '--overlay-scrim'
  | '--error-text' | '--warn-border'
  | '--warn-text' | '--success-text' | '--error-soft' | '--error-border'
  | '--text-on-accent';

/** Area identity slots. An area's colour is a slot name, never a free-form hex (§6.2). */
export type AreaIdentityToken =
  | '--area-1' | '--area-2' | '--area-3' | '--area-4'
  | '--area-5' | '--area-6' | '--area-7' | '--area-8';

export type ColorToken =
  | PositionalColorToken | SurfaceToken | OverlayToken | SemanticColorToken | AreaIdentityToken;

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
