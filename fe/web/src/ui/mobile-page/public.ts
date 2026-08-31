const MOBILE_PAGE_ROOT_EVENT = 'nc:mobile-page-root';
const MOBILE_SECONDARY_EVENT = 'nc:mobile-secondary';

export function requestMobilePageRoot(): void {
  setMobileSecondaryOpen(false);
  window.dispatchEvent(new Event(MOBILE_PAGE_ROOT_EVENT));
}

export function subscribeMobilePageRoot(listener: () => void): () => void {
  window.addEventListener(MOBILE_PAGE_ROOT_EVENT, listener);
  return () => window.removeEventListener(MOBILE_PAGE_ROOT_EVENT, listener);
}

export function setMobileSecondaryOpen(open: boolean): void {
  window.dispatchEvent(new CustomEvent<boolean>(MOBILE_SECONDARY_EVENT, { detail: open }));
}

export function subscribeMobileSecondary(listener: (open: boolean) => void): () => void {
  const receive = (event: Event) => listener((event as CustomEvent<boolean>).detail);
  window.addEventListener(MOBILE_SECONDARY_EVENT, receive);
  return () => window.removeEventListener(MOBILE_SECONDARY_EVENT, receive);
}
