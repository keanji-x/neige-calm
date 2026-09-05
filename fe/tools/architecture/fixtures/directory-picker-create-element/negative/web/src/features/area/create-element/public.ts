import { createElement } from 'react';
import { DirectoryBrowser } from '../../../ui/directory-browser/public.tsx';

export const host = () => createElement(DirectoryBrowser, { initialPath: null });
