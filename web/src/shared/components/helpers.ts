import type { Area } from '../../types';

export function areaOf(areaId: string, areas: Area[]): Area | undefined {
  return areas.find((c) => c.id === areaId);
}
