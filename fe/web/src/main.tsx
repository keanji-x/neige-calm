import { installDemoPolicy, isOnboardingDemo } from './app/onboarding-entry.ts';

const root = document.getElementById('root');
if (!root) throw new Error('Missing #root mount point');
if (isOnboardingDemo(window.location.search)) {
  installDemoPolicy(document);
  void import('./app/onboarding-demo/mount.tsx').then(({ mountOnboardingDemo }) => mountOnboardingDemo(root));
} else void import('./app/auth/production-app.tsx').then(({ mountProductionApp }) => mountProductionApp(root, {
  storage: window.localStorage,
  reload: () => { window.location.reload(); },
  deleteDatabase: (name) => { indexedDB.deleteDatabase(name); },
}));
