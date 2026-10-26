// Rename / entry actions stay off this component; the DOM slots and class names are the CSS contract.

import type { ReactNode } from 'react';
import { DrawerHeader } from '../../../ui/drawer/header.tsx';
import type { CardHeaderPresentation } from '../registry.js';

import { Icon } from '../../../ui/icon/public.tsx';
import { LetterAvatar } from './letter-avatar.tsx';

export function CardHead({
  title,
  status,
  icon,
  className,
  children,
  onDelete,
  deleteLabel,
  header,
}: {
  title?: ReactNode;
  status?: ReactNode;
  icon?: ReactNode;
  className?: string;
  children?: ReactNode;
  onDelete?: () => void;
  deleteLabel?: string;
  header?: CardHeaderPresentation;
}) {
  if (header !== undefined) return <DrawerHeader title={title} status={status} closeLabel={header.hideLabel} onClose={header.onHide}
    onDelete={onDelete} deleteLabel={deleteLabel}>{children}</DrawerHeader>;
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
      {onDelete !== undefined && (
        <button
          className="card-grid-close"
          type="button"
          aria-label={deleteLabel ?? 'Delete'}
          onClick={(event) => {
            event.stopPropagation();
            onDelete();
          }}
          onKeyDown={(event) => event.stopPropagation()}
          onMouseDown={(event) => event.stopPropagation()}
        >
          <Icon name="delete" size="sm" />
        </button>
      )}
    </div>
  );
}
