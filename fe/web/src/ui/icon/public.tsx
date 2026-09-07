import styles from './icon.module.css';

export type IconName =
  | 'chevron-left' | 'chevron-right' | 'arrow-left' | 'arrow-up'
  | 'plus' | 'close' | 'more' | 'chat' | 'notification' | 'folder' | 'file'
  | 'paperclip' | 'pencil';

const paths: Readonly<Record<IconName, readonly string[]>> = Object.freeze({
  'chevron-right': Object.freeze(['M6 3.5 10.5 8 6 12.5']),
  'chevron-left': Object.freeze(['M10 3.5 5.5 8 10 12.5']),
  'arrow-left': Object.freeze(['M13 8H3.5', 'M7 3.5 2.5 8 7 12.5']),
  'arrow-up': Object.freeze(['M8 12.5V3.5', 'M4 7.5 8 3.5l4 4']),
  plus: Object.freeze(['M8 3.5v9', 'M3.5 8h9']),
  close: Object.freeze(['M4 4l8 8', 'M12 4l-8 8']),
  more: Object.freeze(['M3.25 8h.01', 'M8 8h.01', 'M12.75 8h.01']),
  chat: Object.freeze(['M3 3.5h10v7H7l-3.5 2v-2H3z']),
  notification: Object.freeze([
    'M4 11.5h8l-1.2-1.7V7a2.8 2.8 0 0 0-5.6 0v2.8L4 11.5Z',
    'M6.5 12.5a1.5 1.5 0 0 0 3 0',
  ]),
  /* The two filesystem marks (§6.7's set, added for the directory browser).
     They are closed outlines rather than filled shapes because every other
     icon here is line work and a filled folder would read as the heaviest
     mark in the app at the smallest size it is used. The folder's tab is the
     one diagonal in the set; the file's fold is a second path so the corner
     stays a real fold instead of a crease drawn over a rectangle. */
  folder: Object.freeze(['M2 12.5V3.5h4.2l1.6 2H14v7z']),
  file: Object.freeze(['M4 2.5h5l3 3v8H4z', 'M9 2.5v3h3']),
  /* The attach control's mark (#1505 S6).
     A paperclip and not a picture frame: a frame at 16px has about 7×5 of
     interior left after its own 1.5 stroke, and the horizon and sun drawn
     inside it merged into one dark mass — measured, not guessed. The clip is a
     single open stroke with no interior at all, so it survives the size it is
     actually used at, and it is the idiom for "attach" in any case. */
  /* The queue strip's edit mark. A nib and its stroke, no barrel outline:
     the same reason the paperclip below is one open path — at 16px an
     enclosed shape with a 1.5 stroke has no interior left to be an interior. */
  pencil: Object.freeze([
    'M11.5 2.5 13.5 4.5 5.5 12.5 3 13 3.5 10.5z',
    'M10 4 12 6',
  ]),
  paperclip: Object.freeze([
    'M14.29 7.37l-6.13 6.13a4 4 0 0 1-5.66-5.66l6.13-6.13a2.67 2.67 0 0 1 3.77 3.77l-6.13 6.13a1.33 1.33 0 0 1-1.89-1.89l5.66-5.65',
  ]),
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
