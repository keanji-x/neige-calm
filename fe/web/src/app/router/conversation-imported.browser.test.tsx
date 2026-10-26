import '../../styles/entry.css';
import { cleanup, screen } from '@testing-library/react';
import { afterEach, expect, it } from 'vitest';
import { page } from 'vitest/browser';
import { importedFixture } from './conversation-imported-fixture.tsx';

afterEach(()=>{cleanup();document.getElementById('root')?.remove();});
it.each([1280,390])('u2_imported_renderer_provenance_%s',async width=>{
  await page.viewport(width,800);
  const f=importedFixture();
  await page.getByRole('button',{name:/Conversation Daily Planner conversation/}).click();
  await expect.element(page.getByText('Imported saved words',{exact:true})).toBeVisible();
  await expect.element(page.getByText('Imported history · Unknown author',{exact:true})).toBeVisible();
  await expect.element(page.getByText('Imported reply',{exact:true})).toBeVisible();
  expect(document.querySelector('[data-nc-turn="you"]')?.textContent).toBe('Real input');
  expect([...document.querySelectorAll('[data-nc-turn="you"]')].some(node=>node.textContent?.includes('Imported saved words'))).toBe(false);
  expect(screen.getAllByText('Imported saved words',{exact:true})).toHaveLength(1);
  expect(document.querySelector('[data-nc-turn-attachments] img')?.getAttribute('src')).toMatch(/^data:image/);
  await expect.element(page.getByText('Native reply',{exact:true})).toBeVisible();
  for(const status of ['Failed','Interrupted','Completed']){
    await expect.element(page.getByText(`Imported history · ${status}`,{exact:true})).toBeVisible();
  }
  await expect.element(page.getByText(/Imported history · System/)).toBeVisible();
  await expect.element(page.getByText('Imported history · Ran',{exact:true})).toBeVisible();
  expect(screen.queryByText('Send a message to continue.')).toBeNull();
  await page.screenshot({path:`./__screenshots__/u2-imported-${width}.png`});
  f.client.clear();
});
