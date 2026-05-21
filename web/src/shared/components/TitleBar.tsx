import { Icon } from '../../Icon';

// ---------------- TitleBar ----------------

export function TitleBar({
  theme,
  onToggleTheme,
  onOpenSettings,
}: {
  theme: 'light' | 'dark';
  onToggleTheme: () => void;
  /** Open the app-global settings page. Optional so tests / sub-trees that
   *  render the bar without a router don't have to wire it up. */
  onOpenSettings?: () => void;
}) {
  return (
    <header className="bar">
      <div className="name">Neige</div>
      <div className="right">
        {onOpenSettings && (
          <button
            className="go ghost"
            onClick={onOpenSettings}
            title="Settings"
            aria-label="Open settings"
          >
            <Icon n="gear" s={14} />
          </button>
        )}
        <button
          className="go ghost"
          onClick={onToggleTheme}
          title="Toggle theme"
          aria-label={
            theme === 'dark' ? 'Switch to light theme' : 'Switch to dark theme'
          }
        >
          <Icon n={theme === 'dark' ? 'sun' : 'moon'} s={14} />
        </button>
      </div>
    </header>
  );
}
