import { afterEach, expect, it, vi } from 'vitest';
import { isOnboardingDemo } from './onboarding-entry.ts';

afterEach(() => {
  vi.resetModules();
  vi.doUnmock('./onboarding-demo/mount.tsx');
  vi.doUnmock('./auth/production-app.tsx');
  document.body.replaceChildren();
  for (const node of document.head.querySelectorAll('meta[http-equiv="Content-Security-Policy"]')) node.remove();
  window.history.replaceState({}, '', '/');
});

it('recognizes only explicit lesson selectors', () => {
  expect(isOnboardingDemo('?demo=onboarding')).toBe(true);
  expect(isOnboardingDemo('?onboarding=workspace')).toBe(true);
  expect(isOnboardingDemo('?demo=other')).toBe(false);
  expect(isOnboardingDemo('')).toBe(false);
});

it.each(['?demo=onboarding', '?onboarding=workspace'])('boots the isolated Demo without mounting production for %s', async search => {
  vi.resetModules();
  const demo = vi.fn(); const production = vi.fn();
  vi.doMock('./onboarding-demo/mount.tsx', () => ({ mountOnboardingDemo: demo }));
  vi.doMock('./auth/production-app.tsx', () => ({ mountProductionApp: production }));
  document.body.innerHTML = '<div id="root"></div>';
  window.history.replaceState({}, '', `/next/${search}`);
  await import('../main.tsx');
  await vi.waitFor(() => expect(demo).toHaveBeenCalledOnce());
  expect(production).not.toHaveBeenCalled();
  expect(document.head.querySelector('meta[http-equiv="Content-Security-Policy"]')?.getAttribute('content')).toContain("connect-src 'none'");
});

it('keeps normal startup on the production composition', async () => {
  const demo = vi.fn(); const production = vi.fn();
  vi.doMock('./onboarding-demo/mount.tsx', () => ({ mountOnboardingDemo: demo }));
  vi.doMock('./auth/production-app.tsx', () => ({ mountProductionApp: production }));
  document.body.innerHTML = '<div id="root"></div>';
  window.history.replaceState({}, '', '/next/');
  await import('../main.tsx');
  await vi.waitFor(() => expect(production).toHaveBeenCalledOnce());
  expect(demo).not.toHaveBeenCalled();
  expect(document.head.querySelector('meta[http-equiv="Content-Security-Policy"]')).toBeNull();
});
