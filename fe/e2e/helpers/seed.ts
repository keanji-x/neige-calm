import type { APIRequestContext } from '@playwright/test';

export type SeededArea = Readonly<{ id: string; name: string }>;
export type SeededWave = Readonly<{ id: string; title: string }>;

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

export async function createWave(
  request: APIRequestContext,
  areaId: string,
  title = `FE e2e wave ${Date.now()}`,
): Promise<SeededWave> {
  // #1147 S3 — no `cwd`. Omitting it is the *managed workspace* branch:
  // the kernel derives `<workspace-root>/<area>/<wave>` and creates the
  // git repository itself, so the seed works in every environment (docker
  // stack or native server) without the spec having to own a directory.
  // It is also exactly what the new FE's default create sends (see
  // `wave-create.spec.ts`, which pins "no cwd on the wire"), so the seed
  // stays representative. Sending an explicit `cwd` is the *attached*
  // branch, and since S3 the kernel requires that path to already exist
  // and be inside a git work tree — an invented `/tmp/...` path is a 400,
  // and before S3 it was worse: the wave was created but every worker on
  // it died in `git_repo_root_for_wave_cwd`.
  const response = await request.post('/api/waves', {
    data: {
      area_id: areaId,
      title,
      theme: { fg: [216, 219, 226], bg: [15, 20, 24] },
    },
  });
  await requireOk(response, 'createWave: POST /api/waves');
  return await response.json() as SeededWave;
}
