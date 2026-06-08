import { createRoot } from 'react-dom/client';
import { ThemeProvider } from '../app/theme.tsx';
import '../calm.css';
import { ColorContactSheet } from './ColorContactSheet.tsx';

createRoot(document.getElementById('root')!).render(
  <ThemeProvider>
    <ColorContactSheet />
  </ThemeProvider>,
);
