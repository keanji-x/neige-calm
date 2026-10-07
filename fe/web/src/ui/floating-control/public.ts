import styles from './floating-control.module.css';

/** Shared floating surface only; the caller owns semantics, focus and actions. */
export const floatingControlClassName = styles.surface;

/** Inherited surface parameters for composed controls with their own layout. */
export const floatingControlMaterialClassName = styles.material;
