import { useEffect, useRef } from 'react';
import { runOperation } from '../providers/queries.ts';
import { ApiError } from '../../../../core/domain/failure-class.ts';
import type { ApiTransportPort } from '../../../../core/api/types.ts';
import { shareAccessOperation, unlockShareLinkOperation } from '../../../../core/domain/share.ts';
import { useState } from '../../ui/state/public.ts';
import styles from './share-unlock.module.css';

export function ShareUnlockGate({ shareLinkId, transport, retry, initialToken }: Readonly<{
  shareLinkId: string; transport: ApiTransportPort; retry: () => void; initialToken?: string;
}>) {
  const [status, setStatus] = useState<'checking' | 'locked' | 'missing' | 'error'>('checking');
  const [token, setToken] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const initial = useRef<Readonly<{
    shareLinkId: string; transport: ApiTransportPort; request: Promise<UnlockCheck>;
  }> | null>(null);
  useEffect(() => {
    let active = true;
    if (initial.current === null || initial.current.shareLinkId !== shareLinkId || initial.current.transport !== transport) {
      const hash = initialToken ?? takeShareTokenFromHash();
      // Keep the request across StrictMode effect replay, including after the hash is removed.
      initial.current = { shareLinkId, transport, request: checkShareAccess(shareLinkId, transport, hash) };
    }
    void initial.current.request.then(result => {
      if (!active) return;
      if (result.status === 'unlocked') retry();
      else { setStatus(result.status); setError(result.error); }
    });
    return () => { active = false; };
  }, [initialToken, retry, shareLinkId, transport]);
  if (status === 'checking') return null;
  if (status === 'missing') return <main className={styles.page}><div className={styles.card}>
    <p className={styles.error} role="alert">分享链接不存在或已过期。</p>
  </div></main>;
  if (status === 'error') return <main className={styles.page}><div className={styles.card}>
    <p className={styles.error} role="alert">无法检查分享链接。</p><button onClick={retry}>重试</button>
  </div></main>;
  return <main className={styles.page}><div className={styles.card}>
    <h1>打开分享链接</h1><p className={styles.description}>请输入分享者提供的口令。分享链接 24 小时内有效。</p>
    <form className={styles.form} onSubmit={event => {
      event.preventDefault();
      if (busy || token.trim() === '') return;
      setBusy(true); setError(null);
      void runOperation(transport, unlockShareLinkOperation(shareLinkId, token), undefined).then(() => {
        setToken(''); retry();
      }).catch((error: unknown) => {
        const code = httpStatus(error);
        if (code === 404) { setToken(''); setStatus('missing'); return; }
        setError(unlockError(code));
      }).finally(() => setBusy(false));
    }}>
      <label>口令<input type="password" autoComplete="off" value={token} onChange={event => setToken(event.target.value)} /></label>
      <button type="submit" disabled={busy || token.trim() === ''}>解锁</button>
      {error !== null && <p className={styles.error} role="alert">{error}</p>}
    </form>
  </div></main>;
}

// Capture at the browser entry point before session probing; standalone gates use the same fallback.
export function takeShareTokenFromHash(): string {
  const hash = window.location.hash.slice(1);
  if (hash !== '') window.history.replaceState(window.history.state, '', window.location.pathname + window.location.search);
  return hash;
}

function httpStatus(error: unknown): number | null {
  return error instanceof ApiError && 'status' in error.failure ? error.failure.status : null;
}

type UnlockCheck = Readonly<{ status: 'unlocked' | 'locked' | 'missing' | 'error'; error: string | null }>;
async function checkShareAccess(shareLinkId: string, transport: ApiTransportPort, hash: string): Promise<UnlockCheck> {
  try {
    const access = await runOperation(transport, shareAccessOperation(shareLinkId), undefined);
    if (access.status === 'unlocked') return { status: 'unlocked', error: null };
  } catch (error: unknown) {
    return { status: httpStatus(error) === 404 ? 'missing' : 'error', error: null };
  }
  if (hash === '') return { status: 'locked', error: null };
  try {
    await runOperation(transport, unlockShareLinkOperation(shareLinkId, decodeURIComponent(hash)), undefined);
    return { status: 'unlocked', error: null };
  } catch (error: unknown) {
    const code = httpStatus(error);
    return { status: code === 404 ? 'missing' : 'locked', error: unlockError(code) };
  }
}
function unlockError(code: number | null): string {
  return code === 401 ? '口令不正确，请重试。' : code === 429 ? '尝试次数过多，请稍后再试。' : '无法解锁，请重试。';
}
