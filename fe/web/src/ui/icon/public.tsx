import styles from './icon.module.css';

export type IconName =
  | 'chevron-left' | 'chevron-right' | 'arrow-left' | 'arrow-up'
  | 'plus' | 'close' | 'reset';

const paths: Readonly<Record<IconName, readonly string[]>> = Object.freeze({
  'chevron-right': Object.freeze(['M6 3.5 10.5 8 6 12.5']),
  'chevron-left': Object.freeze(['M10 3.5 5.5 8 10 12.5']),
  'arrow-left': Object.freeze(['M13 8H3.5', 'M7 3.5 2.5 8 7 12.5']),
  'arrow-up': Object.freeze(['M8 12.5V3.5', 'M4 7.5 8 3.5l4 4']),
  plus: Object.freeze(['M8 3.5v9', 'M3.5 8h9']),
  close: Object.freeze(['M4 4l8 8', 'M12 4l-8 8']),
  reset: Object.freeze(['M3 3.5v3.25h3.25', 'M3.35 6.5a5 5 0 1 1 .15 4.1']),
});

export function Icon({ name, size = 'md' }: { name: IconName; size?: 'sm' | 'md' }) {
  return (
    <svg
      className={styles[size]}
      viewBox="0 0 16 16"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.5"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
      focusable="false"
    >
      {/* The token sizes the em-like icon box; the line work keeps a 0.85
          optical inset inside it, just as a font's ink sits inside its em box. */}
      <g transform="translate(8 8) scale(0.85) translate(-8 -8)">
        {paths[name].map((path) => <path key={path} d={path} />)}
      </g>
    </svg>
  );
}
