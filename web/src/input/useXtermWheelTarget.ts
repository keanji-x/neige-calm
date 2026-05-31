import { useCallback, useLayoutEffect, useRef } from 'react';
import { useState } from '../shared/state';
import type { XtermViewHandle } from '../XtermView';
import { registerXtermShell, unregisterXtermShell } from './wheelTargets';

export function useXtermWheelTargetRef<T extends XtermViewHandle>() {
  const ref = useRef<T | null>(null);
  const [version, setVersion] = useState(0);

  const setRef = useCallback((node: T | null) => {
    ref.current = node;
    setVersion((n) => n + 1);
  }, []);

  useLayoutEffect(() => {
    const target =
      typeof ref.current?.getWheelTarget === 'function'
        ? ref.current.getWheelTarget()
        : null;
    const shell = target?.root.closest<HTMLElement>('[data-wheel-card]');
    if (!target || !shell) return;
    registerXtermShell(shell, target);
    return () => unregisterXtermShell(shell);
  }, [version]);

  return [ref, setRef] as const;
}
