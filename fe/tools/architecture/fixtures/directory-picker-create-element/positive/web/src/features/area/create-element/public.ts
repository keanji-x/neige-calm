import { createElement } from 'react';
import { DirectoryPanel } from '../../../ui/directory-panel/public.tsx';

export const host = () => createElement(DirectoryPanel, { initialPath: null });
