// <CardHead> — typed slot component for the unified `.card-head` base.
//
// Background: PR #171 introduced a 3-class slot system in `web/src/calm.css`
// (`.card-head` + `.card-head-icon` / `.card-head-title` / `.card-head-status`)
// and dual-classed Codex + Terminal onto it. Issue #178 collapses that
// dual-classing into a typed React component so future card heads can't
// "forget" the contract — slots become TypeScript props.
//
// DOM order is load-bearing: `icon` first (small avatar / glyph block left
// of title), `title` next, `children` escape-hatch in the middle, `status`
// last (so the base's `justify-content: flex-end` on `.card-head-status`
// pins it to the right while the title can claim free space).
//
// Each prop, when defined, renders into its own slot wrapper span which
// owns the slot class (`.card-head-title` etc).
//
// When the caller omits `icon` and the `title` is a plain string, the
// component synthesises a small letter-avatar block (first letter of the
// title's first word, painted on a deterministic hash-of-title palette
// background). The avatar is the canonical card-identity glyph until a
// card opts into a real SVG via `icon`.

import type { ReactNode } from 'react';
import { CloseIcon } from '../shared/components/CloseIcon';

export type CardHeadProps = {
  /** Title content. Rendered inside `<span className="card-head-title">`. */
  title?: ReactNode;
  /** Right-aligned status content. Rendered inside `<span className="card-head-status">`. */
  status?: ReactNode;
  /** Card identity glyph block; overrides the default letter-avatar. */
  icon?: ReactNode;
  /** Additional class names merged with `card-head` on the root. */
  className?: string;
  /**
   * Escape hatch for content that doesn't fit the named slots. Rendered
   * between `title` and `status` so right-aligned status stays pinned.
   */
  children?: ReactNode;
  /**
   * When defined, render a hover-revealed `×` close button absolutely
   * positioned at the head's top-right. Omitted entirely when undefined,
   * so cards that aren't user-deletable (or contexts that own the close
   * affordance elsewhere — e.g. WaveList's row-level button) render no
   * head button. The button stops `mousedown` propagation so a click on
   * it inside an RGL drag handle never initiates a card drag.
   */
  onClose?: () => void;
  /** ARIA label for the close button. Defaults to `'Close'`. */
  closeAriaLabel?: string;
};

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

function DefaultLetterAvatar({ title }: { title: string }) {
  const letter = firstLetter(title);
  if (!letter) return null;
  const idx = hashTitle(title);
  return (
    <span
      className={`card-head-icon card-head-icon--letter card-head-icon--c${idx}`}
      aria-hidden="true"
    >
      {letter}
    </span>
  );
}

/**
 * Compose the slot wrappers + optional escape-hatch children into the
 * shared `.card-head` skeleton. Slot wrappers are only emitted when the
 * corresponding prop is non-undefined — no empty `<span class="…">`
 * placeholders, which keeps the DOM honest and avoids the flex parent's
 * `gap` treating an empty span as a child.
 */
export function CardHead({
  title,
  status,
  icon,
  className,
  children,
  onClose,
  closeAriaLabel,
}: CardHeadProps) {
  const rootClass = className ? `card-head ${className}` : 'card-head';
  // Synthesise the letter-avatar only when the caller didn't pass an icon
  // AND the title is a plain string (non-string titles often wrap rich
  // content where a letter avatar would clash; consumers must opt-in
  // explicitly via `icon` in that case).
  let iconNode: ReactNode = null;
  if (icon !== undefined) {
    iconNode = <span className="card-head-icon">{icon}</span>;
  } else if (typeof title === 'string') {
    iconNode = <DefaultLetterAvatar title={title} />;
  }
  return (
    <div className={rootClass}>
      {iconNode}
      {title !== undefined && <span className="card-head-title">{title}</span>}
      {children}
      {status !== undefined && <span className="card-head-status">{status}</span>}
      {onClose !== undefined && (
        // Structurally last so the flex slots (icon/title/children/status)
        // keep their justified layout — the close button is absolutely
        // positioned out of flow against `.card-head`. `onMouseDown` stops
        // propagation so RGL's drag-handle (which the head usually carries)
        // doesn't initiate a drag on the close click.
        <button
          className="card-grid-close"
          type="button"
          aria-label={closeAriaLabel ?? 'Close'}
          onClick={(e) => {
            e.stopPropagation();
            onClose();
          }}
          onMouseDown={(e) => e.stopPropagation()}
        >
          <CloseIcon />
        </button>
      )}
    </div>
  );
}
