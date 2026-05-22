import { ConnectionIndicator } from './ConnectionIndicator';

// ---------------- TitleBar ----------------
//
// The title bar now only hosts the connection indicator. Theme switching
// moved to the Settings page's Appearance section (Light/Dark/System
// radio); the user menu (Settings, Sign out) is the Sidebar's avatar
// row. See issue #22.

export function TitleBar() {
  return (
    <header className="bar">
      <div className="right">
        <ConnectionIndicator />
      </div>
    </header>
  );
}
