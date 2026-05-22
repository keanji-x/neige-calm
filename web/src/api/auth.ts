/**
 * Auth wire. Posts hit the calm-server origin via the Vite dev proxy.
 * Currently dead code: `main.tsx` deliberately bypasses <AuthGate /> because
 * calm-server hasn't implemented auth yet. The endpoints below match the
 * shape the kernel will expose when it does.
 */
export async function whoami(): Promise<boolean> {
  try {
    const res = await fetch('/api/auth/whoami');
    return res.ok;
  } catch {
    return false;
  }
}

export async function login(token: string): Promise<boolean> {
  const res = await fetch('/login/submit', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ token }),
  });
  return res.ok;
}

export async function logout(): Promise<boolean> {
  try {
    const res = await fetch('/api/auth/logout', { method: 'POST' });
    return res.ok;
  } catch {
    return false;
  }
}
