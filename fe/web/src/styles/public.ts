export type PositionalColorToken =
  | '--bg' | '--paper' | '--hairline' | '--hairline-strong'
  | '--text' | '--text-2' | '--text-3' | '--text-4'
  | '--accent' | '--accent-soft' | '--warn' | '--warn-soft';

export type SurfaceToken =
  | '--surface-rail' | '--surface-card' | '--surface-chip' | '--surface-toggle-overlay'
  | '--surface-panel-head' | '--surface-terminal' | '--surface-code'
  | '--surface-paper' | '--surface-bg' | '--surface-hover-overlay';

export type OverlayToken =
  | '--overlay-hover-faint' | '--overlay-hover' | '--overlay-hover-strong' | '--overlay-active';

export type SemanticColorToken =
  | '--text-label' | '--text-meta' | '--text-decorative'
  | '--success' | '--error' | '--overlay-scrim'
  | '--cal-event-waiting-bg' | '--error-text' | '--warn-border';

export type ColorToken =
  | PositionalColorToken | SurfaceToken | OverlayToken | SemanticColorToken;

export type TypeScaleToken =
  | '--text-xs' | '--text-sm' | '--text-base' | '--text-md'
  | '--text-lg' | '--text-xl' | '--text-display-sm' | '--text-display';

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
  | '--radius-xs' | '--radius-sm' | '--radius-md'
  | '--radius-lg' | '--radius-xl' | '--radius-pill';

export type MotionToken =
  | '--motion-instant' | '--motion-quick' | '--motion-snappy'
  | '--motion-medium' | '--motion-slow' | '--motion-pulse';

export type ScalarToken =
  | TypeScaleToken | LeadingToken | TrackingToken | RadiusToken | SpacingToken | MotionToken;

export type FontToken =
  | '--font-sans' | '--font-serif' | '--font-mono'
  | '--font-display' | '--font-numeric' | '--font-code';

export type ZIndexToken =
  | '--z-base' | '--z-raised' | '--z-sticky' | '--z-overlay' | '--z-modal' | '--z-toast';

export type StyleToken = ColorToken | ScalarToken | FontToken | ZIndexToken;
