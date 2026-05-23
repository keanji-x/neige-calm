// ---------------- ProgressBar ----------------
//
// `running=true` adds the live-pulse class so the bar gets the accent
// shimmer. Callers compute the boolean via `isRunning(wave.lifecycle)`
// from `shared/lifecycle.ts`.

export function ProgressBar({
  value,
  running,
}: {
  value: number;
  running?: boolean;
}) {
  return (
    <div className={'fill ' + (running ? 'running' : '')}>
      <div className="v" style={{ width: value * 100 + '%' }} />
    </div>
  );
}
