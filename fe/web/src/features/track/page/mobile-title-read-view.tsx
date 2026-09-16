import { useLayoutEffect } from 'react';
import { createPortal } from 'react-dom';
import type { EditableTitleProps } from '../../../ui/editable-title/public.tsx';

/** Register only while a read view has a live editor to open. The editor itself
 * remains the single EditableTitle instance, including its draft and guards. */
export function MobileTitleReadView({ controls, host, view, register }: Readonly<{
  controls: Parameters<NonNullable<EditableTitleProps['readView']>>[0];
  host: HTMLElement;
  view: NonNullable<EditableTitleProps['readView']>;
  register: (begin: (() => void) | null) => void;
}>) {
  useLayoutEffect(() => {
    register(controls.beginEditing);
    return () => { register(null); };
  }, [controls.beginEditing, register]);
  return createPortal(view(controls), host);
}
