import { useEffect, useRef } from 'react';
import { useState } from '../state/public.ts';

export function CopyCodeButton({ text }: Readonly<{ text: string }>) {
  const [state, setState] = useState<'idle' | 'copied' | 'failed'>('idle');
  const generation = useRef(0);
  useEffect(() => {
    generation.current += 1;
    setState('idle');
    return () => { generation.current += 1; };
  }, [text, setState]);
  const copy = async () => {
    const request = ++generation.current;
    try {
      await navigator.clipboard.writeText(text);
      if (request === generation.current) setState('copied');
    } catch {
      if (request === generation.current) setState('failed');
    }
  };
  return <>
    <button type="button" onClick={() => { void copy(); }}>Copy code</button>
    <span role="status">{state === 'copied' ? 'Copied' : state === 'failed' ? 'Copy failed' : ''}</span>
  </>;
}
