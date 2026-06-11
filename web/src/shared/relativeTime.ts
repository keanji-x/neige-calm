export function formatUpdatedAt(updatedAt?: number | null): string {
  return formatRelativeTime('Updated', updatedAt);
}

export function formatRelativeTime(
  label: string,
  timestamp?: number | null,
): string {
  const cleanLabel = label.trim() || 'Updated';
  if (
    typeof timestamp !== 'number' ||
    !Number.isFinite(timestamp) ||
    timestamp <= 0
  ) {
    return `${cleanLabel} -`;
  }

  const diffMs = Math.max(0, Date.now() - timestamp);
  const minutes = Math.floor(diffMs / 60_000);
  if (minutes < 1) return `${cleanLabel} just now`;
  if (minutes < 60) return `${cleanLabel} ${minutes}m ago`;

  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${cleanLabel} ${hours}h ago`;

  const days = Math.floor(hours / 24);
  if (days < 30) return `${cleanLabel} ${days}d ago`;

  return `${cleanLabel} ${new Intl.DateTimeFormat(undefined, {
    month: 'short',
    day: 'numeric',
    year: 'numeric',
  }).format(new Date(timestamp))}`;
}
