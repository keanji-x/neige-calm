// Copied from web/src/cards/CardHead.tsx. Rename / entry actions stay off
// this slice; the DOM slots and class names are the CSS contract.

import type { ReactNode } from 'react';

import { Icon } from '../../../ui/icon/public.tsx';
import { LetterAvatar } from './letter-avatar.tsx';

export function CardHead({
  title,
  status,
  icon,
  className,
  children,
  onClose,
  closeAriaLabel,
}: {
  title?: ReactNode;
  status?: ReactNode;
  icon?: ReactNode;
  className?: string;
  children?: ReactNode;
  onClose?: () => void;
  closeAriaLabel?: string;
}) {
  const rootClass = className ? `card-head ${className}` : 'card-head';
  let iconNode: ReactNode = null;
  if (icon !== undefined) {
    iconNode = <span className="card-head-icon">{icon}</span>;
  } else if (typeof title === 'string') {
    iconNode = <LetterAvatar title={title} />;
  }
  return (
    <div
      className={rootClass}
      data-nc-card-drag={className?.includes('card-drag-handle') ? '' : undefined}
    >
      {iconNode}
      {title !== undefined ? <span className="card-head-title">{title}</span> : null}
      {children}
      {status !== undefined && <span className="card-head-status">{status}</span>}
      {onClose !== undefined && (
        <button
          className="card-grid-close"
          type="button"
          aria-label={closeAriaLabel ?? 'Close'}
          onClick={(event) => {
            event.stopPropagation();
            onClose();
          }}
          onKeyDown={(event) => event.stopPropagation()}
          onMouseDown={(event) => event.stopPropagation()}
        >
          <Icon name="close" size="sm" />
        </button>
      )}
    </div>
  );
}
