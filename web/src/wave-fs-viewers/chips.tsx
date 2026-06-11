import type {
  CardRole,
  WaveFsRunStatus,
  WaveLifecycle,
} from '../api/generated-events';

export type ViewerChipTone =
  | 'neutral'
  | 'accent'
  | 'warning'
  | 'success'
  | 'danger';

export function ViewerChip({
  label,
  tone = 'neutral',
}: {
  label: string;
  tone?: ViewerChipTone;
}) {
  return (
    <span className="wave-fs-viewer-chip" data-tone={tone}>
      {label}
    </span>
  );
}

export const runStatusTones = {
  completed: 'success',
  failed: 'danger',
  running: 'accent',
  requested: 'accent',
  unknown: 'neutral',
} satisfies Record<WaveFsRunStatus, ViewerChipTone>;

export const waveLifecycleTones = {
  draft: 'neutral',
  planning: 'accent',
  dispatching: 'accent',
  working: 'accent',
  blocked: 'warning',
  reviewing: 'accent',
  done: 'success',
  canceled: 'danger',
  failed: 'danger',
} satisfies Record<WaveLifecycle, ViewerChipTone>;

export const cardRoleTones = {
  worker: 'neutral',
  spec: 'accent',
  reportcard: 'success',
} satisfies Record<CardRole, ViewerChipTone>;

export function verdictTone(status: string): ViewerChipTone {
  switch (status) {
    case 'accepted':
    case 'approved':
    case 'completed':
    case 'done':
      return 'success';
    case 'rejected':
    case 'failed':
      return 'danger';
    case 'blocked':
      return 'warning';
    default:
      return 'neutral';
  }
}
