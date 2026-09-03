// Copied from web/src/cards/LetterAvatar.tsx. Class names are load-bearing
// for the calm.css rules ported into styles/track-grid.css.

const ICON_PALETTE_SIZE = 8;

function hashTitle(value: string): number {
  let hash = 5381;
  for (let index = 0; index < value.length; index += 1) {
    hash = ((hash << 5) + hash + value.charCodeAt(index)) | 0;
  }
  return Math.abs(hash) % ICON_PALETTE_SIZE;
}

function firstLetter(value: string): string | null {
  const trimmed = value.trim();
  if (trimmed.length === 0) return null;
  const match = trimmed.match(/\S/u);
  return match ? match[0].toUpperCase() : null;
}

function semanticClass(title: string): string | null {
  const key = title.trim().toLowerCase();
  if (key === 'codex') return 'card-head-icon--codex';
  if (key === 'claude') return 'card-head-icon--claude';
  return null;
}

export function LetterAvatar({ title }: { title: string }) {
  const letter = firstLetter(title);
  if (!letter) return null;
  const idx = hashTitle(title);
  const semantic = semanticClass(title);
  return (
    <span
      className={`card-head-icon card-head-icon--letter card-head-icon--c${String(idx)}${semantic ? ` ${semantic}` : ''}`}
      aria-hidden="true"
    >
      {letter}
    </span>
  );
}
