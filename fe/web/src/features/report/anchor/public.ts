export const ARRIVAL_ATTRIBUTE = 'data-nc-arrived';

type ArrivalElement = HTMLElement & { __neigeReportArrivalRun?: number };

function scrollBounds(element: HTMLElement, root: Document): Readonly<{ top: number; bottom: number }> {
  const view = root.defaultView;
  for (let parent = element.parentElement; parent !== null; parent = parent.parentElement) {
    const overflow = view?.getComputedStyle(parent).overflowY ?? '';
    if (!/(auto|scroll|overlay)/.test(overflow) || parent.scrollHeight <= parent.clientHeight) continue;
    const box = parent.getBoundingClientRect();
    return { top: box.top, bottom: box.bottom };
  }
  return { top: 0, bottom: view?.innerHeight ?? Number.POSITIVE_INFINITY };
}

function markAfterSmoothScroll(element: ArrivalElement, root: Document, run: number): void {
  const view = root.defaultView;
  let lastTop: number | null = null;
  let stableFrames = 0;
  let finished = false;
  const finish = () => {
    if (finished || element.__neigeReportArrivalRun !== run) return;
    finished = true;
    view?.clearTimeout(fallback);
    element.setAttribute(ARRIVAL_ATTRIBUTE, '');
  };
  const fallback = view?.setTimeout(finish, 2_500);
  const sample = () => {
    if (finished || element.__neigeReportArrivalRun !== run) return;
    const box = element.getBoundingClientRect();
    const bounds = scrollBounds(element, root);
    const visible = box.bottom > bounds.top && box.top < bounds.bottom;
    stableFrames = visible && lastTop !== null && Math.abs(box.top - lastTop) < 0.5
      ? stableFrames + 1
      : 0;
    lastTop = box.top;
    if (stableFrames >= 3) finish();
    else requestAnimationFrame(sample);
  };
  requestAnimationFrame(sample);
}

export function revealReportAnchor(
  anchorId: string,
  root: Document = document,
  behavior: ScrollBehavior = 'auto',
): void {
  const element: ArrivalElement | null = root.getElementById(anchorId);
  if (element === null) return;

  // A reader-supplied fragment must not unfold details outside a report.
  const inReport = element.closest('[data-nc-report]') !== null;
  if (inReport) {
    for (const details of element.querySelectorAll('details')) details.open = true;
  }
  for (
    let ancestor = inReport ? element.closest('details') : null;
    ancestor !== null;
    ancestor = ancestor.parentElement?.closest('details') ?? null
  ) {
    ancestor.open = true;
  }

  const reducedMotion = root.defaultView?.matchMedia('(prefers-reduced-motion: reduce)').matches ?? false;
  const effectiveBehavior = reducedMotion ? 'auto' : behavior;
  const run = (element.__neigeReportArrivalRun ?? 0) + 1;
  element.__neigeReportArrivalRun = run;
  element.removeAttribute(ARRIVAL_ATTRIBUTE);
  element.scrollIntoView({ block: 'start', behavior: effectiveBehavior });
  if (effectiveBehavior === 'smooth') {
    markAfterSmoothScroll(element, root, run);
    return;
  }
  requestAnimationFrame(() => {
    if (element.__neigeReportArrivalRun === run) element.setAttribute(ARRIVAL_ATTRIBUTE, '');
  });
}
