import '../../styles/entry.css';
import { cleanup, render } from '@testing-library/react';
import { afterEach, expect, it } from 'vitest';
import { page } from 'vitest/browser';
import { SettingsPane } from './public.tsx';

afterEach(cleanup);
it('preserves native heading-to-description spacing without an active guide', async () => {
  await page.viewport(1000, 800);
  render(<SettingsPane category="about" title="Build details" lede="Read the current version."><p>Details</p></SettingsPane>);
  const heading = page.getByRole('heading', { name: 'Build details' }).element().getBoundingClientRect();
  const description = page.getByText('Read the current version.', { exact: true }).element().getBoundingClientRect();
  expect(description.top - heading.bottom).toBe(12);
});
