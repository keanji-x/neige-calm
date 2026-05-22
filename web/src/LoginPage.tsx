import { useState } from './shared/state';
import { login } from './api/auth';

/**
 * Owner login form (issue #189). Posts {username, password} to
 * /api/auth/login via the Vite proxy; on success the server sets the
 * httpOnly `calm-session` cookie and returns the whoami payload.
 *
 * We reload the page on success so the SessionProvider remounts and
 * re-runs the whoami probe (whose 200 result drives the router mount).
 * Re-issuing whoami inside this component would technically work, but
 * the reload is the simplest path that guarantees every persisted /
 * in-memory cache also gets a fresh start under the new identity —
 * matches what the ServerCompatGate's bust path does for the same
 * structural reason.
 */
export function LoginPage() {
  const [username, setUsername] = useState('');
  const [password, setPassword] = useState('');
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (submitting || !username.trim() || !password) return;
    setSubmitting(true);
    setError(null);
    try {
      const result = await login(username.trim(), password);
      if (!result) {
        setError('Wrong username or password.');
        setSubmitting(false);
        return;
      }
      window.location.reload();
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Sign-in failed.');
      setSubmitting(false);
    }
  };

  return (
    <div className="login-page">
      <div className="login-card">
        <div className="login-eyebrow">Neige · Calm</div>
        <h1 className="login-title">Sign in.</h1>
        <form onSubmit={submit}>
          <input
            className="login-input"
            type="text"
            placeholder="Username"
            value={username}
            onChange={(e) => setUsername(e.target.value)}
            // Single-purpose login screen; autofocus the first field for
            // expected UX. There's no surrounding context to skip past.
            // eslint-disable-next-line jsx-a11y/no-autofocus
            autoFocus
            autoComplete="username"
            spellCheck={false}
          />
          <input
            className="login-input"
            type="password"
            placeholder="Password"
            value={password}
            onChange={(e) => setPassword(e.target.value)}
            autoComplete="current-password"
            spellCheck={false}
          />
          {error && <div className="login-error">{error}</div>}
          <button
            className="go"
            type="submit"
            disabled={submitting || !username.trim() || !password}
            style={{ width: '100%', justifyContent: 'center', marginTop: 4 }}
          >
            {submitting ? 'Signing in…' : 'Sign in'}
          </button>
        </form>
      </div>
    </div>
  );
}
