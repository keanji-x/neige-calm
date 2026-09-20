/** The typed confirmation: the heaviest rung of the confirmation ladder, used for exactly one operation (deleting an area). */

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

/** Case-sensitive, no Unicode normalisation: look-alike strings must not pass. */
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

/** No error message while the text does not match, and no placeholder: it would suggest the name can be copied out of it. */
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
