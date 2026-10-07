import { useCallback, type RefObject } from 'react';
import { useState } from '../../ui/state/public.ts';
import { useSpringPresence } from '../../ui/motion/presence.ts';

/** Keep the navigation and its focus layer through the shared presence trajectory. */
export function useMobileNavigationPresence(host: RefObject<HTMLDivElement | null>, onClosed: () => void, present: boolean) {
  const [closing, setClosing] = useState(false);
  if (!present && closing) setClosing(false);
  useSpringPresence(host, null, present && !closing, present, true, onClosed,
    value => ({ transform: `translateX(${(1 - value) * 100}%)` }));
  const close = useCallback(() => { setClosing(true); }, []);
  const cancel = useCallback(() => { setClosing(false); }, []);
  return { close, cancel };
}
