import { flushSync } from 'react-dom';

/** Shared by the message before and the composer field after; one Edit at a time owns it. */
const EDITED_MESSAGE = 'nc-edited-message';

/**
 * Edit's motion (#1923): a copy of the message moves into the composer as one View Transition, the browser's default
 * 250 ms, while the message itself stays (it carries no name in the new state, so it fades back in where it was).
 * Visual only: `update` makes the same change either way, and runs at once where there is no transition (no API,
 * reduced motion, no message on screen). With one, it runs inside the transition's update callback a frame later.
 */
export function moveIntoComposer(message: HTMLElement | null, update: () => void): void {
  if (message === null || !('startViewTransition' in document)
    || window.matchMedia('(prefers-reduced-motion: reduce)').matches) {
    update();
    return;
  }
  /* The composer shares the drawer card with the transcript. */
  const composer = message.closest('[data-nc-drawer]')?.querySelector<HTMLElement>('[data-nc-composer]') ?? null;
  let field: HTMLElement | null = null;
  message.style.viewTransitionName = EDITED_MESSAGE;
  const transition = document.startViewTransition(() => {
    flushSync(update);
    message.style.viewTransitionName = '';
    field = composer?.querySelector<HTMLElement>('[contenteditable="true"], textarea') ?? null;
    if (field !== null) field.style.viewTransitionName = EDITED_MESSAGE;
  });
  void transition.finished.finally(() => {
    if (field !== null) field.style.viewTransitionName = '';
  });
}
