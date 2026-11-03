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
  // Omitted cwd allocates a Managed locator and empty canonical carrier; first input prepares it.
  const response = await request.post('/api/tracks', {
    data: {
      area_id: areaId,
      planner_provider: 'codex',
      title,
      theme: { fg: [216, 219, 226], bg: [15, 20, 24] },
    },
  });
  await requireOk(response, 'createTrack: POST /api/tracks');
  return await response.json() as SeededTrack;
}

/** Real input settles before the stale working snapshot races Stop. */
export async function activatePlanner(request: APIRequestContext, trackId: string): Promise<void> {
  const detail = await (await request.get(`/api/tracks/${trackId}`)).json() as { cards: { id: string; payload: { planner_harness?: boolean } }[] };
  const planner = detail.cards.find(card => card.payload.planner_harness === true);
  if (planner === undefined) throw new Error('Track has no Planner');
  const response = await request.post(`/api/cards/${planner.id}/planner/input`, {
    headers: { 'Idempotency-Key': crypto.randomUUID() }, data: { text: 'fake-reply: Stop race prepared' },
  });
  await requireOk(response, 'activatePlanner');
  const deadline = Date.now() + 20_000;
  while (Date.now() < deadline) {
    const run = await (await request.get(`/api/cards/${planner.id}/planner/run?contract=conversation_v1`)).json() as {
      input_contract: string; phase: string; worker_session_id: string | null;
      pending_inputs: unknown[]; active_input: unknown;
    };
    if (run.input_contract === 'canonical' && run.phase === 'turn_completed'
      && run.worker_session_id !== null && run.pending_inputs.length === 0 && run.active_input === null) return;
    await new Promise(resolve => setTimeout(resolve, 50));
  }
  throw new Error('First input did not reach its actual END');
}
