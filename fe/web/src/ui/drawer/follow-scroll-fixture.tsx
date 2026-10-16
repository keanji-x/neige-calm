import { useLayoutEffect, useRef } from 'react';
import { useState } from '../state/public.ts';
import { createScrollFollower } from './follow-scroll.ts';
import { restoreDrawerFocus } from './restore-focus.ts';

export function Pane({ growth = 0, restoreFocus = false, name = 'pane', selectAtAttach = false }: { growth?: number; restoreFocus?: boolean; name?: string; selectAtAttach?: boolean }) {
  const pane = useRef<HTMLDivElement | null>(null);
  const content = useRef<HTMLDivElement | null>(null);
  const [follower] = useState(() => createScrollFollower({ bottomSlack: 64, onAwayChange: () => {} }));
  useLayoutEffect(() => follower.attach(pane.current!, content.current!), [follower]);
  useLayoutEffect(() => {
    if (restoreFocus) restoreDrawerFocus(content.current!.querySelector<HTMLAnchorElement>('a')!);
  }, [growth, restoreFocus]);
  return <>
    <button onClick={() => follower.followToEnd()}>Return to latest</button>
    <div ref={pane} data-testid={name} data-nc-drawer-scroll="" style={{ height: 300, width: 400, overflow: 'auto' }}>
      <div ref={content}>
        <p ref={element => {
          if (element !== null && selectAtAttach) {
            const range = document.createRange(); range.selectNodeContents(element);
            const selection = window.getSelection()!; selection.removeAllRanges(); selection.addRange(range);
          }
        }}><a href="#history">A unique historical search target</a></p>
        {Array.from({ length: 60 }, (_, index) => <p key={index}>Paragraph {index}. {'A long passage of text. '.repeat(12)}</p>)}
        <a href="#tail">Latest link</a>
        <details><summary>Latest details</summary><p style={{ height: 400 }}>The detail grew.</p></details>
        <div style={{ height: growth }} />
      </div>
    </div>
    <input aria-label="Outside composer" />
  </>;
}
