import { useState } from './shared/state';
import { login } from './api/auth';

/**
 * Posts to calm-server's /login/submit via the vite proxy; on success the
 * session cookie is set scoped to the Calm origin and we reload so AuthGate
 * sees the new whoami() result.
 */
export function LoginPage() {
  const [token, setToken] = useState('');
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!token.trim() || submitting) return;
    setSubmitting(true);
    setError(null);
    try {
      const ok = await login(token.trim());
      if (!ok) throw new Error('Invalid token.');
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
        <p className="login-hint">
          Paste your token. Generate one with{' '}
          <code>calm-server auth rotate</code>.
        </p>
        <form onSubmit={submit}>
          <input
            className="login-input"
            type="password"
            placeholder="Token"
            value={token}
            onChange={(e) => setToken(e.target.value)}
            autoFocus
            autoComplete="off"
            spellCheck={false}
          />
          {error && <div className="login-error">{error}</div>}
          <button
            className="go"
            type="submit"
            disabled={submitting || !token.trim()}
            style={{ width: '100%', justifyContent: 'center', marginTop: 4 }}
          >
            {submitting ? 'Signing in…' : 'Sign in'}
          </button>
        </form>
      </div>
    </div>
  );
}
