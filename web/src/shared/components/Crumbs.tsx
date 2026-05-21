import { Fragment } from 'react';

// ---------------- Crumbs ----------------

interface CrumbItem {
  label: string;
  onClick?: () => void;
}

export function Crumbs({ items }: { items: CrumbItem[] }) {
  return (
    <div className="crumbs">
      {items.map((it, i) => {
        const last = i === items.length - 1;
        return (
          <Fragment key={i}>
            {last ? (
              <span className="now">{it.label}</span>
            ) : (
              <button type="button" className="crumb-link" onClick={it.onClick}>
                {it.label}
              </button>
            )}
            {!last && <span>·</span>}
          </Fragment>
        );
      })}
    </div>
  );
}
