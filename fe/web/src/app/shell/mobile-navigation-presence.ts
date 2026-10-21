import { useCallback, type RefObject } from 'react';
import { useState } from '../../ui/state/public.ts';
import { useEdgePresence } from '../../ui/motion/edge-presence.ts';
import { readSurfaceFrame } from '../../ui/motion/geometry.ts';

/** Retain exit paint through Motion; interaction ends at the close commit. */
export function useMobileNavigationPresence(host: RefObject<HTMLDivElement | null>, frame: RefObject<HTMLDivElement | null>, onClosed: () => void, present: boolean): Readonly<{ close: () => void; cancel: () => void; interactive: boolean }> {
  const [closing, setClosing] = useState(false);
  if (!present && closing) setClosing(false);
  useEdgePresence({ frame: { ref: frame }, part: { element: host, readFrame: () => readSurfaceFrame(frame.current) },
    edge: 'inline-end', open: present && !closing, present, enabled: true, onExited: onClosed });
  const close = useCallback(() => { setClosing(true); }, []);
  const cancel = useCallback(() => { setClosing(false); }, []);
  return { close, cancel, interactive: present && !closing };
}
