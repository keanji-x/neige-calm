export const ARRIVAL_ATTRIBUTE = 'data-nc-arrived';

export function revealReportAnchor(anchorId: string, root: Document = document): void {
  const element = root.getElementById(anchorId);
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

  element.scrollIntoView({ block: 'start', behavior: 'auto' });
  element.removeAttribute(ARRIVAL_ATTRIBUTE);
  requestAnimationFrame(() => { element.setAttribute(ARRIVAL_ATTRIBUTE, ''); });
}
