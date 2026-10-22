import type { CommandBindings } from '../../features/chat/thread/composer-command.ts';
import type { TrackClosedControl } from '../../features/track/page/closed-control.ts';

/** The app binds server capabilities to the existing Track controls. Chat reads no Track policy. */
export function trackCommandBindings({ canReopen, canClose, closed, stop }: Readonly<{
  canReopen: boolean;
  canClose: boolean;
  closed: Pick<TrackClosedControl, 'pending' | 'setClosed'>;
  stop?: () => void;
}>): CommandBindings {
  return {
    open: { unavailableReason: !canReopen ? 'This Track cannot be reopened.' : closed.pending ? 'A Track update is pending.' : undefined,
      execute: () => closed.setClosed('reopen') },
    close: { unavailableReason: !canClose ? 'This Track cannot be closed.' : closed.pending ? 'A Track update is pending.' : undefined,
      execute: () => closed.setClosed('close') },
    stop: { unavailableReason: stop === undefined ? 'There is no turn to stop.' : undefined, execute: () => stop?.() },
  };
}
