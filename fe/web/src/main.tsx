import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';

const root = document.getElementById('root');

if (!root) throw new Error('Missing #root mount point');

createRoot(root).render(<StrictMode><main>Neige Calm frontend</main></StrictMode>);
