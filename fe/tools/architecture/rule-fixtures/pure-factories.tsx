import { createElement, forwardRef, lazy, memo } from 'react';
declare const Comp: () => null;
declare const fn: () => null;
export const Memo = memo(Comp);
export const Forward = forwardRef(fn);
export const Lazy = lazy(() => import('./lazy-target'));
export const Element = createElement('div');
export const tag = Symbol('t');
export const raw = String.raw`value`;
