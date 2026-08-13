import type { APIRequestContext } from '@playwright/test';

export type SeededCove = Readonly<{ id: string; name: string }>;
export type SeededWave = Readonly<{ id: string; title: string }>;

async function requireOk(response: Awaited<ReturnType<APIRequestContext['post']>>, operation: string): Promise<void> {
  if (response.ok()) return;
  const body = await response.text().catch(() => '<unreadable body>');
  throw new Error(`${operation} → ${response.status()} ${response.statusText()}: ${body}`);
}

export async function createCove(
  request: APIRequestContext,
  name = `FE e2e cove ${Date.now()}`,
): Promise<SeededCove> {
  const response = await request.post('/api/coves', { data: { name, color: '#6a8' } });
  await requireOk(response, 'createCove: POST /api/coves');
  return await response.json() as SeededCove;
}

export async function createWave(
  request: APIRequestContext,
  coveId: string,
  title = `FE e2e wave ${Date.now()}`,
): Promise<SeededWave> {
  const response = await request.post('/api/waves', {
    data: {
      cove_id: coveId,
      title,
      cwd: `/tmp/fe-e2e-${Date.now()}-${coveId}`,
      attach_folder: true,
      theme: { fg: [216, 219, 226], bg: [15, 20, 24] },
    },
  });
  await requireOk(response, 'createWave: POST /api/waves');
  return await response.json() as SeededWave;
}
