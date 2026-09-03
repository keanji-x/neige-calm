/**
 * §6.13 — the typed confirmation. The third and heaviest rung of §4.3's
 * confirmation ladder, and the product has exactly **one** operation on it:
 * deleting an area, which cascades to every wave inside it.
 *
 * "One" means one *operation*, not one entry point: the rail's area row and the
 * area page header both open this, with the same copy and the same strength. An
 * operation with two confirmation strengths is the real hole.
 *
 * Used more widely it stops being a protection and becomes the new normal.
 */

import { useEffect, useRef, type RefObject } from 'react';

import { useState } from '../state/public.ts';
import styles from './typed-confirm.module.css';

export type TypedConfirmCopy = Readonly<{
  title: string;
  consequence: string;
  prompt: string;
  confirmLabel: string;
}>;

export type TypedConfirm = Readonly<{
  value: string;
  setValue: (value: string) => void;
  inputRef: RefObject<HTMLInputElement | null>;
  matches: boolean;
}>;

/**
 * Case-sensitive, no Unicode normalisation. Normalising would let two strings
 * that merely *look* alike through, and "you actually read the name" is the
 * entire value of this gate.
 */
export function useTypedConfirm(expected: string): TypedConfirm {
  const [value, setValue] = useState('');
  const inputRef = useRef<HTMLInputElement | null>(null);
  // A different target is a different question; carrying the old answer over
  // could arm the button before the user has read the new name.
  useEffect(() => { setValue(''); }, [expected]);
  return {
    value,
    setValue,
    inputRef,
    matches: expected !== '' && value === expected,
  };
}

/**
 * Two sentences with different typography, which is why `deleteAreaCopy`
 * returns four fields rather than one `description`: a single slot cannot carry
 * both, and writing the prompt at each call site is exactly what INV-DUP-010
 * exists to prevent.
 *
 * There is no error message when the text does not match — the user is still
 * typing, not failing. And no placeholder in the input: a placeholder would
 * suggest the name can be copied out of it.
 */
export function TypedDeleteBody({ copy, expected, value, inputRef, onChange }: {
  copy: TypedConfirmCopy;
  expected: string;
  value: string;
  inputRef: RefObject<HTMLInputElement | null>;
  onChange: (value: string) => void;
}) {
  return (
    <div className={styles.body}>
      <p className={styles.consequence}>{copy.consequence}</p>
      <p className={styles.prompt}>
        Type <span className={styles.name}>{expected}</span> to confirm.
      </p>
      <input
        ref={inputRef}
        type="text"
        className={styles.input}
        aria-label={copy.prompt}
        value={value}
        onChange={(event) => onChange(event.target.value)}
      />
    </div>
  );
}
