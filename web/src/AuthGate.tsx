import { useEffect } from 'react';
import { useState } from './shared/state';
import { whoami } from './api/auth';
import { LoginPage } from './LoginPage';

/**
 * Renders children only when the calm-server session cookie is valid.
 * Until whoami() resolves we render nothing — keeps the page calm and avoids
 * a flash of the dashboard before the redirect to <LoginPage />.
 */
export function AuthGate({ children }: { children: React.ReactNode }) {
  const [authed, setAuthed] = useState<boolean | null>(null);

  useEffect(() => {
    let cancelled = false;
    whoami().then((ok) => {
      if (!cancelled) setAuthed(ok);
    });
    return () => { cancelled = true; };
  }, []);

  if (authed === null) return null;
  if (!authed) return <LoginPage />;
  return <>{children}</>;
}
