import styles from './floating-control.module.css';
export { AnchoredHint, supportsAnchoredHints } from './anchored-hint.tsx';

/** Shared floating surface only; the caller owns semantics, focus and actions. */
export const floatingControlClassName = styles.surface;

/** Inherited surface parameters for composed controls with their own layout. */
export const floatingControlMaterialClassName = styles.material;

export { GuideProvider, GuideAnchor, guideRevealSlot, useGuideTargetActive } from './guided-tour.tsx';
export { parseGuideCatalog, type GuideCatalog } from './guide-catalog.ts';
