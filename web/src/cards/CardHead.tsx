// <CardHead> — typed slot component for the unified `.card-head` base.
//
// Background: PR #171 introduced a 4-class slot system in `web/src/calm.css`
// (`.card-head` + `.card-head-decor` / `.card-head-title` / `.card-head-status`)
// and dual-classed Codex + Terminal onto it. Issue #178 collapses that
// dual-classing into a typed React component so future card heads can't
// "forget" the contract — slots become TypeScript props.
//
// DOM order is load-bearing: `decor` first (left of title), `title` next,
// `children` escape-hatch in the middle, `status` last (so the base's
// `justify-content: flex-end` on `.card-head-status` pins it to the right
// while the title can claim free space).
//
// Each prop, when defined, renders into its own slot wrapper span which
// owns the slot class (`.card-head-title` etc). Consumers stack additional
// styling via the content — e.g. Terminal passes `<span className="term-title">…</span>`
// as `title`, and this component wraps it in `<span className="card-head-title">`.
//
// No `actions` slot — YAGNI per #178's non-goals. Add when the first
// consumer needs it.

import type { ReactNode } from 'react';

export type CardHeadProps = {
  /** Title content. Rendered inside `<span className="card-head-title">`. */
  title?: ReactNode;
  /** Right-aligned status content. Rendered inside `<span className="card-head-status">`. */
  status?: ReactNode;
  /** Left-of-title decoration (e.g. Terminal's 3 dots). Rendered inside `<span className="card-head-decor">`. */
  decor?: ReactNode;
  /** Additional class names merged with `card-head` on the root. */
  className?: string;
  /**
   * Escape hatch for content that doesn't fit the three named slots.
   * Rendered between `title` and `status` (so right-aligned status
   * stays pinned to the right edge).
   */
  children?: ReactNode;
};

/**
 * Compose the three slot wrappers + optional escape-hatch children into
 * the shared `.card-head` skeleton. Slot wrappers are only emitted when
 * the corresponding prop is non-undefined — no empty `<span class="…">`
 * placeholders, which keeps the DOM honest and avoids accidental layout
 * (the flex parent's `gap` would treat an empty span as a child).
 */
export function CardHead({
  title,
  status,
  decor,
  className,
  children,
}: CardHeadProps) {
  const rootClass = className ? `card-head ${className}` : 'card-head';
  return (
    <div className={rootClass}>
      {decor !== undefined && <span className="card-head-decor">{decor}</span>}
      {title !== undefined && <span className="card-head-title">{title}</span>}
      {children}
      {status !== undefined && <span className="card-head-status">{status}</span>}
    </div>
  );
}
