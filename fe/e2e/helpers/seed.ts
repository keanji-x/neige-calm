import type { APIRequestContext } from '@playwright/test';

export type SeededArea = Readonly<{ id: string; name: string }>;
export type SeededTrack = Readonly<{ id: string; title: string }>;

async function requireOk(response: Awaited<ReturnType<APIRequestContext['post']>>, operation: string): Promise<void> {
  if (response.ok()) return;
  const body = await response.text().catch(() => '<unreadable body>');
  throw new Error(`${operation} → ${response.status()} ${response.statusText()}: ${body}`);
}

export async function createArea(
  request: APIRequestContext,
  name = `FE e2e area ${Date.now()}`,
): Promise<SeededArea> {
  const response = await request.post('/api/areas', { data: { name, color: '#6a8' } });
  await requireOk(response, 'createArea: POST /api/areas');
  return await response.json() as SeededArea;
}

export async function createTrack(
  request: APIRequestContext,
  areaId: string,
  title = `FE e2e track ${Date.now()}`,
): Promise<SeededTrack> {
  // No `cwd`: the managed-workspace branch, so the kernel creates the repository itself. An
  // explicit `cwd` must already exist inside a git work tree, so an invented `/tmp/...` path is a 400.
  const response = await request.post('/api/tracks', {
    data: {
      area_id: areaId,
      title,
      theme: { fg: [216, 219, 226], bg: [15, 20, 24] },
    },
  });
  await requireOk(response, 'createTrack: POST /api/tracks');
  return await response.json() as SeededTrack;
}
