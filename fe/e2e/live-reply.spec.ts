import { expect, test } from '@playwright/test';

import { createArea } from './helpers/seed.js';

test.use({ contextOptions: { reducedMotion: 'reduce' } });

/* #1923 S2: the fixture app-server streams a reply scripted by a line of the message
 * (`osc-probe-child`, `fake-reply-hold:`) and holds the turn open until it is interrupted. */
test('shows a streamed reply before its turn ends, and the same text once stored', async ({ page, request }) => {
  const area = await createArea(request, `Live reply ${Date.now()}`);
  try {
    // Keyed first_message is the existing canonical Codex entry on main.
    const created = await request.post('/api/tracks', {headers:{'Idempotency-Key':`p2-live-${Date.now()}`},data:{
      area_id:area.id,planner_provider:'codex',title:'Canonical live reply',first_message:'fake-reply-hold: Streamed |before |the end',
      theme:{fg:[216,219,226],bg:[15,20,24]},
    }});
    expect(created.status()).toBe(201);
    const track = await created.json() as {id:string};
    await page.goto(`/next/track/${track.id}`);
    const opener = page.getByRole('button', { name: /^Conversation fake-reply-hold:/ });
    await opener.click();
    const composer = page.getByRole('combobox', { name: 'Message' });
    await expect(composer).toHaveAttribute('contenteditable', 'true');

    const transcript = page.locator('[data-nc-thread]');
    const reply = transcript.locator('[data-nc-turn="agent"]');
    /* The turn is held open, so this text can only be the live copy. */
    await expect(reply).toHaveText('Streamed before the end');
    await expect(page.getByRole('button', { name: 'Stop' })).toBeVisible();

    const detail = await (await request.get(`/api/tracks/${track.id}`)).json() as {cards:{id:string;payload:{planner_harness?:boolean}}[]};
    const card = detail.cards.find(card=>card.payload.planner_harness === true);
    expect(card).toBeDefined();
    const runPath = `/api/cards/${card!.id}/planner/run?contract=conversation_v1`;
    expect(await (await request.get(runPath)).json()).toMatchObject({input_contract:'canonical',terminal_outcome:null});
    await page.getByRole('button', { name: 'Stop' }).click();
    await expect.poll(async()=>await (await request.get(runPath)).json() as unknown).toMatchObject({terminal_outcome:{status:'interrupted',card_id:card!.id}});
    await expect(page.locator('[data-nc-current-meta]')).toContainText('Interrupted');
    await expect(reply).toHaveCount(1);
    await expect(reply).toHaveText('Streamed before the end');

    await page.screenshot({path:test.info().outputPath('canonical-interrupted.png'),fullPage:true});

    /* After a refresh only the stored `_partial` row can draw it. */
    await page.reload();
    await expect(opener.or(transcript).first()).toBeVisible();
    if (await opener.isVisible()) await opener.click();
    await expect(page.locator('[data-nc-current-meta]')).toContainText('Interrupted');
    await expect(reply).toHaveCount(1);
    await expect(reply).toHaveText('Streamed before the end');
  } finally {
    await request.delete(`/api/areas/${area.id}`);
  }
});
