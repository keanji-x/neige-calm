import { useId } from 'react';
import { useState } from '../../../ui/state/public.ts';
import styles from './login-page.module.css';
import type { SessionIdentity } from '../../../../../core/api/auth.ts';

export type LoginPageProps = Readonly<{
  login: (username: string, password: string) => Promise<SessionIdentity | null>;
  reload: () => void;
}>;

export function LoginPage({ login, reload }: LoginPageProps) {
  const prefix = useId();
  const [username, setUsername] = useState('');
  const [password, setPassword] = useState('');
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState<string | null>(null);

  return <main className={styles.page}>
    <form className={styles.form} onSubmit={(event) => {
      event.preventDefault();
      if (submitting) return;
      setSubmitting(true); setError(null);
      void login(username, password).then((result) => {
        if (!result) { setError('Wrong username or password.'); setSubmitting(false); return; }
        // A reload resets every persisted and in-memory cache under the new identity (#189).
        reload();
      }).catch((cause: unknown) => {
        setError(cause instanceof Error && cause.message ? cause.message : 'Sign-in failed.');
        setSubmitting(false);
      });
    }}>
      <h1>Sign in</h1>
      {error !== null && <p role="alert" className={styles.error}>{error}</p>}
      <label htmlFor={`${prefix}-username`}>Username</label>
      {/* eslint-disable-next-line jsx-a11y/no-autofocus -- Single-purpose login screen intentionally starts at its first field. */}
      <input id={`${prefix}-username`} name="username" autoFocus autoComplete="username" value={username} onChange={(event) => setUsername(event.target.value)} />
      <label htmlFor={`${prefix}-password`}>Password</label>
      <input id={`${prefix}-password`} name="password" type="password" autoComplete="current-password" value={password} onChange={(event) => setPassword(event.target.value)} />
      <button type="submit" disabled={submitting}>{submitting ? 'Signing in…' : 'Sign in'}</button>
    </form>
  </main>;
}
