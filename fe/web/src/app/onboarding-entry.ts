export const ONBOARDING_DEMO_URL = '/next/?demo=onboarding';

/** Select composition before any session, network or production event service is mounted. */
export function isOnboardingDemo(search: string): boolean {
  const query = new URLSearchParams(search);
  return query.get('demo') === 'onboarding' || query.get('onboarding') === 'workspace';
}

/** Defense in depth for the lifetime of this document. Exit reloads the normal document. */
export function installDemoPolicy(document: Document): void {
  const policy = document.createElement('meta');
  policy.httpEquiv = 'Content-Security-Policy';
  policy.content = "connect-src 'none'; frame-src data: blob:; img-src data: blob:; object-src 'none'; form-action 'none'; base-uri 'none'";
  document.head.append(policy);
}
