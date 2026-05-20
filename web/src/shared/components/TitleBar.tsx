import { Icon } from '../../Icon';

// ---------------- TitleBar ----------------

export function TitleBar({
  theme,
  onToggleTheme,
}: {
  theme: 'light' | 'dark';
  onToggleTheme: () => void;
}) {
  return (
    <div className="bar">
      <div className="name">Neige</div>
      <div className="right">
        <button className="go ghost" onClick={onToggleTheme} title="Toggle theme">
          <Icon n={theme === 'dark' ? 'sun' : 'moon'} s={14} />
        </button>
      </div>
    </div>
  );
}
