import { Icon } from '../../Icon';
import { ConnectionIndicator } from './ConnectionIndicator';

// ---------------- TitleBar ----------------

export function TitleBar({
  theme,
  onToggleTheme,
}: {
  theme: 'light' | 'dark';
  onToggleTheme: () => void;
}) {
  return (
    <header className="bar">
      <div className="right">
        <ConnectionIndicator />
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
