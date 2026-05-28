// <LetterAvatar> — the canonical card-identity glyph.
//
// A small rounded-square avatar painted with the first letter of a title's
// first word, on a deterministic hash-of-title palette background. This is
// the default `.card-head-icon` a card shows until it opts into a real SVG
// icon, extracted here so the AddPanel menu can render the SAME glyph for
// its builtin entries ("terminal" → "T", "codex" → "C") without each call
// site re-rolling the hash + palette + DOM contract.
//
// The rendered DOM is load-bearing: `calm.css` rules
// (`.card-head-icon`, `.card-head-icon--letter`, `.card-head-icon--c{0..7}`)
// hang off these exact classes. `CardHead` delegates to this component so
// there is a single source of truth for the glyph's markup.

const ICON_PALETTE_SIZE = 8;

function hashTitle(s: string): number {
  // djb2: cheap deterministic string hash. The avatar colour is purely a
  // visual fingerprint, not a security property, so collisions are fine.
  let h = 5381;
  for (let i = 0; i < s.length; i++) {
    h = ((h << 5) + h + s.charCodeAt(i)) | 0;
  }
  return Math.abs(h) % ICON_PALETTE_SIZE;
}

function firstLetter(s: string): string | null {
  const trimmed = s.trim();
  if (trimmed.length === 0) return null;
  // Match the first Unicode-friendly character of the first word.
  const m = trimmed.match(/\S/u);
  return m ? m[0].toUpperCase() : null;
}

function semanticClass(title: string): string | null {
  const key = title.trim().toLowerCase();
  if (key === 'codex') return 'card-head-icon--codex';
  if (key === 'claude') return 'card-head-icon--claude';
  return null;
}

/**
 * Render the letter-avatar glyph for `title`, or `null` when the title is
 * blank (no letter to draw). Output is byte-identical to what `CardHead`'s
 * private avatar produced before this extraction:
 * `<span class="card-head-icon card-head-icon--letter card-head-icon--c{idx}" aria-hidden>`.
 */
export function LetterAvatar({ title }: { title: string }) {
  const letter = firstLetter(title);
  if (!letter) return null;
  const idx = hashTitle(title);
  const semantic = semanticClass(title);
  return (
    <span
      className={`card-head-icon card-head-icon--letter card-head-icon--c${idx}${semantic ? ` ${semantic}` : ''}`}
      aria-hidden="true"
    >
      {letter}
    </span>
  );
}
