export type RecoveryPhase = 'recovering' | 'offline' | 'syncing' | 'connected' | 'login' | 'update' | 'paused';
export type RecoveryPermit = Readonly<{ generation: number }>;
export type RecoveryState = Readonly<{
  generation: number; phase: RecoveryPhase; detail: string; failedAt: number | null; retryAt: number | null;
}>;
export class RecoveryAccess {
  private state: RecoveryState = { generation: 0, phase: 'recovering', detail: '', failedAt: null, retryAt: null };
  private readonly listeners = new Set<() => void>();
  readonly read = (): RecoveryState => this.state;
  readonly subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener); return () => { this.listeners.delete(listener); };
  };
  change(phase: RecoveryPhase, detail = '', failedAt: number | null = null, retryAt: number | null = null): void {
    this.state = { ...this.state, phase, detail, failedAt, retryAt };
    for (const listener of this.listeners) listener();
  }
  invalidate(phase: RecoveryPhase, detail = ''): void {
    this.state = { ...this.state, generation: this.state.generation + 1 };
    this.change(phase, detail);
  }
  capture(): RecoveryPermit {
    if (this.state.phase !== 'connected') throw new Error('连接恢复后请重新操作；离线操作不会自动发送。');
    return Object.freeze({ generation: this.state.generation });
  }
  check(permit: RecoveryPermit, write = true): void {
    if (permit.generation !== this.state.generation ||
      (write ? this.state.phase !== 'connected' : !['syncing', 'connected'].includes(this.state.phase))) {
      throw new Error('连接状态已改变，请重新操作。');
    }
  }
}

export function recoveryDelay(attempt: number, random: number): number {
  return Math.round(Math.min(30_000, 500 * 2 ** Math.min(attempt, 6)) * (0.75 + Math.max(0, Math.min(1, random)) * 0.5));
}
