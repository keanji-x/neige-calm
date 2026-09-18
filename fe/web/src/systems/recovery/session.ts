import { sha256 } from '@noble/hashes/sha256';
import { bytesToHex, utf8ToBytes } from '@noble/hashes/utils';
import type { ScanContext, ScanInput } from '../../../../core/domain/recovery/scan.ts';
import type { ScanPairingPort } from './scan.ts';
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
type RecoveryAttempt = Readonly<{ controller: AbortController; generation: number }>;

/** Owns one cancelable identity→version attempt. Neither timers nor network callbacks grant authority. */
export class RecoverySession {
  readonly access: RecoveryAccess;
  identity: SessionIdentity | null = null;
  version: RecoveryVersion | null = null;
  scopeRevision = 0;
  private presentation: RecoveryContext | null = null;
  private controller: AbortController | null = null;
  private releaseCancellation: (() => void) | null = null;
  private timer: ReturnType<typeof setTimeout> | null = null;
  private stable: ReturnType<typeof setTimeout> | null = null;
  private attempt = 0;
  private stopped = true;
  private marker: string | null = null;
  private storageFault = false;
  private readonly ports: RecoverySessionPorts;
  private scan: Readonly<{ context: ScanContext; pairing: ScanPairingPort }> | null = null;
  private scanRequired = false;
  readonly scanOnly: boolean;
  constructor(ports: RecoverySessionPorts, scan?: Readonly<{ input: ScanInput; pairing: ScanPairingPort }>) {
    this.ports = ports; this.access = ports.access;
    this.scanOnly = scan !== undefined && scan.input.kind !== 'absent';
    this.scanRequired = this.scanOnly;
    if (scan?.input.kind === 'scan') this.scan = { context: scan.input.context, pairing: scan.pairing };
    try {
      const raw = ports.storage.getItem(logoutMarkerKey());
      if (raw !== null) {
        const record = JSON.parse(raw) as { schemaVersion?: unknown; fingerprint?: unknown };
        if (record.schemaVersion !== 1 || typeof record.fingerprint !== 'string' || !/^[a-f0-9]{64}$/.test(record.fingerprint)) throw new Error('Invalid logout marker');
        this.marker = record.fingerprint;
      }
    } catch { this.storageFault = true; }
    if (this.blocked()) this.access.change('login', this.storageFault ? '无法读取退出状态，请检查设备存储。' : this.scanOnly ? '正在完成本次扫码配对。' : '已在本机退出。配对后请明确验证本次会话。');
  }
  blocked(): boolean { return this.marker !== null || this.storageFault || this.scanRequired; }
  private cancel(): void {
    this.releaseCancellation?.(); this.releaseCancellation = null;
    this.controller?.abort(); this.controller = null;
    if (this.timer !== null) clearTimeout(this.timer);
    if (this.stable !== null) clearTimeout(this.stable);
    this.timer = null; this.stable = null;
  }
  private beginAttempt(explicit: boolean, signal?: AbortSignal): RecoveryAttempt {
    this.cancel(); this.access.invalidate(explicit ? 'login' : 'recovering');
    const controller = new AbortController(); this.controller = controller;
    const generation = this.access.read().generation;
    const cancel = () => {
      if (this.controller !== controller || this.access.read().generation !== generation) return;
      this.cancelAuthentication();
    };
    signal?.addEventListener('abort', cancel, { once: true });
    this.releaseCancellation = () => signal?.removeEventListener('abort', cancel);
    if (signal?.aborted) cancel();
    return { controller, generation };
  }
  /** Retire the previous explicit intent before any login POST or pairing proof.
   * The capability retains this owner's exact attempt through POST and proof. */
  beginAuthentication(signal?: AbortSignal) {
    signal?.throwIfAborted();
    if (this.stopped) throw new Error('恢复会话已停止。');
    if (this.scanOnly && this.scanRequired) throw new Error('本次扫码证明未完成，请重新扫码。');
    const attempt = this.beginAttempt(true, signal);
    const owns = () => this.controller === attempt.controller && this.access.read().generation === attempt.generation;
    return Object.freeze({
      signal: attempt.controller.signal,
      verify: (expectedSession?: string) => owns() && !attempt.controller.signal.aborted
        ? this.probe(true, expectedSession, attempt) : Promise.resolve(null),
      cancel: () => { if (owns()) this.cancelAuthentication(); },
    });
  }
  cancelAuthentication = (): void => { this.scan = null; this.cancel(); this.access.invalidate('login'); };
  start(): void {
    this.stopped = false;
    const scan = this.scan; this.scan = null;
    if (scan !== null) { void this.probe(true, undefined, this.beginAttempt(false), scan); return; }
    if (!this.blocked()) this.retry();
  }
  stop(): void { this.stopped = true; this.scan = null; this.cancel(); this.access.invalidate(this.blocked() ? 'login' : 'paused'); }
  pause(): void { this.scan = null; this.cancel(); this.access.invalidate(this.blocked() ? 'login' : 'paused'); }
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
  private async deadline<T>(operation: (signal: AbortSignal) => Promise<T>, controller: AbortController, notAfter = Infinity): Promise<T> {
    const remaining = Math.min(8000, notAfter - Date.now());
    if (remaining <= 0 || controller.signal.aborted) throw new Error('扫码已取消或过期，请重新扫码。');
    let timer: ReturnType<typeof setTimeout> | undefined;
    let abort: (() => void) | undefined;
    try {
      return await Promise.race([operation(controller.signal), new Promise<never>((_, reject) => {
        abort = () => reject(new Error('恢复已取消'));
        controller.signal.addEventListener('abort', abort, { once: true });
        timer = setTimeout(() => { controller.abort(); reject(new Error('服务器暂不可达')); }, remaining);
      })]);
    } finally { if (timer !== undefined) clearTimeout(timer); if (abort !== undefined) controller.signal.removeEventListener('abort', abort); }
  }
  private async probe(explicit: boolean, expectedSession?: string, attempt?: RecoveryAttempt,
    scan?: Readonly<{ context: ScanContext; pairing: ScanPairingPort }>): Promise<SessionIdentity | null> {
    const { controller, generation } = attempt ?? this.beginAttempt(explicit);
    if (this.stopped || controller.signal.aborted || this.controller !== controller) return null;
    let identityAccepted = false;
    let identityVerified = false;
    const acceptIdentity = () => {
      if (explicit) {
        this.ports.storage.removeItem(logoutMarkerKey());
        this.marker = null;
      }
      identityAccepted = true; this.scanRequired = false;
      // Rendering the accepted session unmounts its form; that is not cancellation.
      this.releaseCancellation?.(); this.releaseCancellation = null;
    };
    const current = () => !this.stopped && !controller.signal.aborted && this.access.read().generation === generation;
    try {
      let expectedFingerprint: string | undefined;
      const pairingDeadline = scan?.context.deadline ?? Infinity;
      const checkDeadline = () => { if (Date.now() >= pairingDeadline) throw new Error('二维码已过期，请重新扫码。'); };
      if (scan !== undefined) {
        if (this.storageFault) throw new Error('无法读取退出状态，请检查设备存储。');
        await this.deadline(signal => scan.pairing.claim(scan.context, signal), controller, pairingDeadline);
        if (!current()) return null;
        checkDeadline();
        expectedFingerprint = await this.deadline(signal => scan.pairing.redeem(scan.context, signal), controller, pairingDeadline);
        if (!current()) return null;
        checkDeadline();
      }
      const identity = await this.deadline(this.ports.identity, controller, pairingDeadline);
      if (!current()) return null;
      checkDeadline();
      if (explicit) {
        if (this.storageFault) throw new Error('无法读取退出状态，请检查设备存储。');
        if (expectedFingerprint !== undefined && fingerprint(identity.sessionId) !== expectedFingerprint) throw new Error('实际会话与本次扫码配对不符，请重新扫码。');
        if (expectedSession !== undefined && identity.sessionId !== expectedSession) throw new Error('登录会话已改变，请重新登录。');
        if (this.marker !== null && fingerprint(identity.sessionId) === this.marker) throw new Error('仍是已退出的旧会话，请重新配对。');
      }
      identityVerified = true;
      const version = await this.deadline(this.ports.version, controller);
      if (!current()) return null;
      acceptIdentity();
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
        (this.version !== null && (this.version.dbInstanceId !== version.dbInstanceId
          || this.version.syncEventVersion !== version.syncEventVersion)) ||
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
      // A verified identity with an unavailable version keeps normal recovery,
      // but caller cancellation never clears the local logout denial.
      if (identityVerified && !identityAccepted) {
        try { acceptIdentity(); }
        catch (failure) {
          this.access.change('login', failure instanceof Error ? failure.message : '验证未成功，请重试。');
          return null;
        }
      }
      if ((explicit && !identityAccepted) || this.blocked()) {
        this.access.change('login', error instanceof Error ? error.message : '验证未成功，请重试。'); return null;
      }
      const delay = recoveryDelay(this.attempt++, Math.random());
      const now = Date.now();
      this.access.change('offline', '服务器暂不可达', now, now + delay);
      if (this.ports.online() && this.ports.visible()) this.timer = setTimeout(this.retry, delay);
      return null;
    } finally {
      if (this.controller === controller) {
        this.releaseCancellation?.(); this.releaseCancellation = null;
        this.controller = null;
      }
    }
  }
  async verifyNewSession(expectedSession?: string, signal?: AbortSignal): Promise<SessionIdentity | null> {
    if (this.stopped || signal?.aborted || (this.scanOnly && this.scanRequired)) return null;
    const attempt = this.beginAuthentication(signal);
    try { return await attempt.verify(expectedSession); }
    finally { attempt.cancel(); }
  }
  async signOut(): Promise<void> {
    const identity = this.identity;
    this.scan = null;
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
