import { selectionSourceLabel } from './task-selection.js';
import { expect, it } from 'vitest';
import { providerPresentation, taskProvider } from './provider-presentation.js';
it('normalizes historical aliases and uses the existing agent glyph for Antigravity', () => {
  expect(taskProvider('agent', 'antigravity')).toBe('antigravity');
  expect(taskProvider('codex')).toBe('codex');
  expect(taskProvider('claude')).toBe('claude');
  expect(taskProvider('terminal')).toBeNull();
  expect(providerPresentation('antigravity')).toEqual({ label: 'Antigravity', icon: 'agent' });
  expect(providerPresentation(null)).toBeNull();
});


it('distinguishes reported provider defaults, unreported defaults and historical absence', () => {
  expect(selectionSourceLabel('provider_default_reported')).toBe('Provider default');
  expect(selectionSourceLabel('provider_default_unreported')).toBe('Provider default · 未报告');
  expect(selectionSourceLabel(undefined)).toBe('未记录');
});
