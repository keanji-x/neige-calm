import type { WaveStatus } from '../../types';

// ---------------- ProgressBar ----------------

export function ProgressBar({
  value,
  status,
}: {
  value: number;
  status?: WaveStatus;
}) {
  return (
    <div className={'fill ' + (status === 'running' ? 'running' : '')}>
      <div className="v" style={{ width: value * 100 + '%' }} />
    </div>
  );
}
