import { cleanup, render, screen } from '@testing-library/react';
import { afterEach, expect, it } from 'vitest';
import { page } from 'vitest/browser';
import { PluginConfigPane } from './plugin-config.tsx';
import '../../styles/entry.css';

afterEach(() => { cleanup(); delete document.documentElement.dataset.theme; });

it.each(['light', 'dark'] as const)('keeps one focus edge on text and numeric configuration fields in %s', async (theme) => {
  document.documentElement.dataset.theme = theme;
  render(<PluginConfigPane pluginId="example" pluginName="Example" enabled
    detail={{ id: 'example', version: '1', enabled: true, state: 'running',
      config_schema: { type: 'object', properties: { title: { type: 'string' }, retries: { type: 'integer' } } },
      user_config: { title: 'Example', retries: 3 }, effective_config: { title: 'Example', retries: 3 } }}
    loadError={null} onRetryLoad={() => {}} onBack={() => {}}
    onSave={() => Promise.resolve({ ok: true })}
    onApplyRestart={() => Promise.resolve({ saved: true, restart: { rejection: null, state: 'running' } })} />);
  for (const width of [1080, 390]) {
    await page.viewport(width, 844);
    for (const name of ['title', 'retries']) {
      const input = screen.getByLabelText(name);
      input.focus();
      expect(document.activeElement).toBe(input);
      expect(getComputedStyle(input).boxShadow).toBe('none');
      expect(getComputedStyle(input).borderBottomWidth).toBe('0px');
      const wrapper = input.parentElement!;
      await expect.poll(() => getComputedStyle(wrapper).boxShadow).not.toBe('none');
    }
  }
});
