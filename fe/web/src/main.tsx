import { mountProductionApp } from './app/auth/production-app.tsx';

const root = document.getElementById('root');
if (!root) throw new Error('Missing #root mount point');
mountProductionApp(root, {
  storage: window.localStorage,
  submittedStorage: () => window.sessionStorage,
  reload: () => { window.location.reload(); },
  deleteDatabase: (name) => { indexedDB.deleteDatabase(name); },
});
