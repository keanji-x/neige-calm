import type { Cove } from '../../types';

export function coveOf(coveId: string, coves: Cove[]): Cove | undefined {
  return coves.find((c) => c.id === coveId);
}

export function timeOfDay(): string {
  const h = new Date().getHours();
  if (h < 5)  return 'Late night';
  if (h < 12) return 'Morning';
  if (h < 17) return 'Afternoon';
  if (h < 21) return 'Evening';
  return 'Night';
}
