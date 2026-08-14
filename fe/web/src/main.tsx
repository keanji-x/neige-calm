import { mountProductionApp } from './app/production-app.tsx';

const root = document.getElementById('root');
if (!root) throw new Error('Missing #root mount point');
mountProductionApp(root, {
  storage: window.localStorage,
  reload: () => { window.location.reload(); },
  deleteDatabase: (name) => { indexedDB.deleteDatabase(name); },
});
