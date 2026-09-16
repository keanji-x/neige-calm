import { sha256 } from '@noble/hashes/sha256';
import { bytesToHex, utf8ToBytes } from '@noble/hashes/utils';
import type { SessionIdentity } from '../../../../core/api/auth.ts';
import { RecoveryAccess, recoveryDelay } from '../../../../core/domain/recovery/access.ts';
import { logoutMarkerKey, recoveryContextKey, decodeRecoveryContext, recoveryPage, type RecoveryContext, type RecoveryScroll } from '../../../../core/domain/recovery/context.ts';

export type RecoveryVersion = Readonly<{ webCompatVersion: number; minWebCompatVersion: number; syncEventVersion: number; dbInstanceId: string }>;
export type RecoveryStorage = Pick<Storage, 'getItem' | 'setItem' | 'removeItem'>;
export type RecoverySessionPorts = Readonly<{
  access: RecoveryAccess; storage: RecoveryStorage; origin: string; compatibleVersion: number;
  identity(signal: AbortSignal): Promise<SessionIdentity>;
  version(signal: AbortSignal): Promise<RecoveryVersion>;
  logout(signal: AbortSignal): Promise<unknown>;
  clear(): void; online(): boolean; visible(): boolean;
  adoptScope(scope: string): void;
}>;
function fingerprint(sessionId: string): string { return bytesToHex(sha256(utf8ToBytes(sessionId))); }

/** Owns one cancelable identity→version attempt. Neither timers nor network callbacks grant authority. */
export class RecoverySession {
  readonly access: RecoveryAccess;
  identity: SessionIdentity | null = null;
  version: RecoveryVersion | null = null;
  scopeRevision = 0;
  private presentation: RecoveryContext | null = null;
  private controller: AbortController | null = null;
  private timer: ReturnType<typeof setTimeout> | null = null;
  private stable: ReturnType<typeof setTimeout> | null = null;
  private attempt = 0;
  private stopped = true;
  private marker: string | null = null;
  private storageFault = false;
  private readonly ports: RecoverySessionPorts;
  constructor(ports: RecoverySessionPorts) {
    this.ports = ports; this.access = ports.access;
    try {
      const raw = ports.storage.getItem(logoutMarkerKey());
      if (raw !== null) {
        const record = JSON.parse(raw) as { schemaVersion?: unknown; fingerprint?: unknown };
        if (record.schemaVersion !== 1 || typeof record.fingerprint !== 'string' || !/^[a-f0-9]{64}$/.test(record.fingerprint)) throw new Error('Invalid logout marker');
        this.marker = record.fingerprint;
      }
    } catch { this.storageFault = true; }
    if (this.blocked()) this.access.change('login', this.storageFault ? '无法读取退出状态，请检查设备存储。' : '已在本机退出。配对后请明确验证本次会话。');
  }
  blocked(): boolean { return this.marker !== null || this.storageFault; }
  private cancel(): void {
    this.controller?.abort(); this.controller = null;
    if (this.timer !== null) clearTimeout(this.timer);
    if (this.stable !== null) clearTimeout(this.stable);
    this.timer = null; this.stable = null;
  }
  start(): void { this.stopped = false; if (!this.blocked()) this.retry(); }
  stop(): void { this.stopped = true; this.cancel(); this.access.invalidate(this.blocked() ? 'login' : 'paused'); }
  pause(): void { this.cancel(); this.access.invalidate(this.blocked() ? 'login' : 'paused'); }
  retry = (): void => {
    if (this.stopped || this.blocked() || this.access.read().phase === 'update' || this.access.read().phase === 'login') return;
    // A retry button joins the current flight; lifecycle invalidation cancels it first.
    if (this.controller !== null) return;
    if (!this.ports.visible()) { this.pause(); return; }
    if (!this.ports.online()) { this.access.invalidate('offline'); return; }
    void this.probe(false);
  };
  resume = (): void => { if (this.blocked() || this.access.read().phase === 'login' || this.access.read().phase === 'update') return; this.cancel(); this.access.invalidate('recovering'); this.retry(); };
  unauthorized = (): void => {
    this.cancel(); this.access.invalidate('login', '需要重新登录');
    this.identity = null; this.version = null; this.scopeRevision++; this.clear();
  };
  private clear(): void {
    this.presentation = null;
    this.ports.clear();
    try { this.ports.storage.removeItem(recoveryContextKey()); } catch { /* presentation is best effort */ }
  }
  private async deadline<T>(operation: (signal: AbortSignal) => Promise<T>, controller: AbortController): Promise<T> {
    let timer: ReturnType<typeof setTimeout> | undefined;
    try {
      return await Promise.race([operation(controller.signal), new Promise<never>((_, reject) => {
        const abort = () => reject(new Error('恢复已取消'));
        controller.signal.addEventListener('abort', abort, { once: true });
        timer = setTimeout(() => { controller.abort(); reject(new Error('服务器暂不可达')); }, 8000);
      })]);
    } finally { if (timer !== undefined) clearTimeout(timer); }
  }
  private async probe(explicit: boolean, expectedSession?: string): Promise<SessionIdentity | null> {
    this.cancel(); this.access.invalidate('recovering');
    const generation = this.access.read().generation;
    const controller = new AbortController(); this.controller = controller;
    const current = () => !this.stopped && !controller.signal.aborted && this.access.read().generation === generation;
    try {
      const identity = await this.deadline(this.ports.identity, controller);
      if (!current()) return null;
      if (explicit) {
        if (this.storageFault) throw new Error('无法读取退出状态，请检查设备存储。');
        if (expectedSession !== undefined && identity.sessionId !== expectedSession) throw new Error('登录会话已改变，请重新登录。');
        if (this.marker !== null && fingerprint(identity.sessionId) === this.marker) throw new Error('仍是已退出的旧会话，请重新配对。');
        this.ports.storage.removeItem(logoutMarkerKey());
        this.marker = null;
      }
      const version = await this.deadline(this.ports.version, controller);
      if (!current()) return null;
      if (version.minWebCompatVersion > this.ports.compatibleVersion || version.webCompatVersion < this.ports.compatibleVersion) {
        this.access.change('update', version.minWebCompatVersion > this.ports.compatibleVersion ? '请更新 Neige App' : '请更新电脑端 Neige');
        return null;
      }
      let stored = null;
      try {
        const raw = this.ports.storage.getItem(recoveryContextKey());
        stored = decodeRecoveryContext(raw, this.ports.origin);
        if (raw !== null && stored === null) this.ports.storage.removeItem(recoveryContextKey());
      } catch { /* presentation unavailable */ }
      if ((this.identity !== null && this.identity.userId !== identity.userId) ||
        (this.version !== null && this.version.dbInstanceId !== version.dbInstanceId) ||
        (stored !== null && (stored.scope.userId !== identity.userId || stored.scope.dbInstanceId !== version.dbInstanceId))) {
        this.clear(); this.scopeRevision++;
        stored = null;
      }
      this.presentation = this.identity === null ? stored : null;
      this.ports.adoptScope(JSON.stringify([this.ports.origin, identity.userId, version.dbInstanceId]));
      this.identity = identity; this.version = version;
      this.access.change('syncing');
      this.stable = setTimeout(() => { if (current()) this.attempt = 0; }, 30_000);
      return identity;
    } catch (error) {
      if (!current() && this.access.read().generation !== generation) return null;
      if (this.stopped) return null;
      const unauthorized = typeof error === 'object' && error !== null && 'failure' in error &&
        typeof error.failure === 'object' && error.failure !== null && 'kind' in error.failure && error.failure.kind === 'unauthorized';
      if (unauthorized) { this.unauthorized(); return null; }
      if (explicit || this.blocked()) {
        this.access.change('login', error instanceof Error ? error.message : '验证未成功，请重试。'); return null;
      }
      const delay = recoveryDelay(this.attempt++, Math.random());
      const now = Date.now();
      this.access.change('offline', '服务器暂不可达', now, now + delay);
      if (this.ports.online() && this.ports.visible()) this.timer = setTimeout(this.retry, delay);
      return null;
    } finally { if (this.controller === controller) this.controller = null; }
  }
  verifyNewSession = async (expectedSession?: string): Promise<SessionIdentity | null> => {
    if (this.controller !== null || this.stopped) return null;
    return this.probe(true, expectedSession);
  };
  async signOut(): Promise<void> {
    const identity = this.identity;
    this.cancel(); this.access.invalidate('login');
    let message = '已在本机退出。';
    if (identity !== null) {
      try {
        this.marker = fingerprint(identity.sessionId);
        this.ports.storage.setItem(logoutMarkerKey(), JSON.stringify({ schemaVersion: 1, fingerprint: this.marker }));
      } catch { this.storageFault = true; message = '无法保存退出状态，不能保证重开后仍退出。'; }
    }
    this.identity = null; this.version = null; this.scopeRevision++; this.clear();
    this.access.change('login', message);
    // No deferred logout queue: make at most one bounded attempt while online.
    if (identity !== null && this.ports.online()) {
      const controller = new AbortController();
      try { await this.deadline(this.ports.logout, controller); } catch { /* local denial remains authoritative */ }
    }
  }
  takePresentation(): RecoveryContext | null { const value = this.presentation; this.presentation = null; return value; }
  remember(path: string, search: string, scroll: readonly RecoveryScroll[] = []): void {
    if (!this.identity || !this.version || !['connected', 'syncing'].includes(this.access.read().phase)) return;
    try { this.ports.storage.setItem(recoveryContextKey(), JSON.stringify({ schemaVersion: 1,
      scope: { origin: this.ports.origin, userId: this.identity.userId, dbInstanceId: this.version.dbInstanceId }, page: recoveryPage(path, search), scroll }));
    } catch { /* losing a presentation hint cannot revoke the current session */ }
  }
  events(state: 'connecting' | 'connected' | 'disconnected'): void {
    if (!['syncing', 'connected'].includes(this.access.read().phase)) return;
    if (state === 'connected') this.access.change('connected');
    else if (this.access.read().phase === 'connected') { this.access.invalidate('syncing'); }
  }
}
