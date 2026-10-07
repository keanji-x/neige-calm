import cjkStyles from './noto-sc-face.generated.module.css';
import styles from './mobile-font.module.css';

/** Scoped mobile typeface; desktop typography keeps its existing configuration. */
export const mobileFontClassName = `${styles.mobile} ${cjkStyles.faceRegistry}`;
