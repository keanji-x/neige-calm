import { Link } from '@tanstack/react-router';
import { ConnectionIndicator } from './ConnectionIndicator';

// ---------------- TitleBar ----------------
//
// The title bar now only hosts the connection indicator. Theme switching
// moved to the Settings page's Appearance section (Light/Dark/System
// radio); the user menu (Settings, Sign out) is the Sidebar's avatar
// row. See issue #22.
//
// The "Report design" link is a temporary fixture tied to issue #594 —
// opens the full-window iframe preview of `web/public/_design/Report.html`.
// It can be removed when the design view ships as a real ViewMode.

export function TitleBar() {
  return (
    <header className="bar">
      <Link
        to="/_design"
        style={{
          fontFamily: 'inherit',
          fontSize: 11,
          fontWeight: 500,
          color: 'var(--text-2)',
          textDecoration: 'none',
          padding: '4px 10px',
          borderRadius: 6,
          border: '1px dashed var(--hairline-strong)',
          letterSpacing: '0.02em',
        }}
      >
        Report design ↗
      </Link>
      <ConnectionIndicator />
    </header>
  );
}
