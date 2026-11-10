import type { Page, Request, Route } from '@playwright/test';

/** The allowed URL is supplied after the create response identifies the new conversation. */
export async function captureFirstMessageErrors(page: Page, runUrl: () => string | undefined): Promise<() => Promise<string[]>> {
  const pageErrors: string[] = [];
  const consoleErrors: { text: string; url: string }[] = [];
  const failures: Promise<{ url: string; method: string; status: number; body: string | null }>[] = [];
  const served = new WeakSet<Request>();
  const inFlight: Promise<void>[] = [];
  const captureErrors: string[] = [];
  page.on('console', message => {
    if (message.type() === 'error') consoleErrors.push({ text: message.text(), url: message.location().url });
  });
  page.on('pageerror', error => pageErrors.push(error.message));
  page.on('response', response => {
    if (response.status() < 400 || served.has(response.request())) return;
    // An unavailable body is evidence we cannot classify, never permission to ignore it.
    failures.push(response.text().then(body => ({
      url: response.url(), method: response.request().method(), status: response.status(), body,
    }), () => ({
      url: response.url(), method: response.request().method(), status: response.status(), body: null,
    })));
  });

  // Only these same-origin reads need bytes that Chromium may discard after a query abort.
  const matches = (url: URL) => url.origin === new URL(page.url()).origin
    && /^\/api\/cards\/[^/]+\/planner\/run$/.test(url.pathname)
    && url.searchParams.get('contract') === 'conversation_v1';
  const handler = (route: Route): Promise<void> => {
    const request = route.request();
    const work = (async () => {
      if (request.method() !== 'GET') { await route.fallback(); return; }
      // Keep the browser's method and all headers (including cookies). The server executes this
      // original request once; redirects remain for the browser to follow as separate requests.
      const response = await route.fetch({ headers: await request.allHeaders(), maxRedirects: 0, maxRetries: 0 });
      if (response.status() >= 400) {
        const body = await response.text();
        failures.push(Promise.resolve({ url: request.url(), method: request.method(), status: response.status(), body }));
        served.add(request);
      }
      await route.fulfill({ response });
    })().catch(async error => {
      captureErrors.push(`Run-read capture failed for ${request.url()}: ${String(error)}`);
      try { await route.abort('failed'); }
      catch (abortError) { captureErrors.push(`Run-read abort failed: ${String(abortError)}`); }
    });
    inFlight.push(work);
    return work;
  };
  await page.route(matches, handler);

  return async () => {
    await page.unroute(matches, handler);
    await Promise.all(inFlight);
    const errors: string[] = [];
    const allowedResources = new Map<string, number>();
    for (const failure of failures) {
      const { url, method, status, body } = await failure;
      const declaredConflict = url === runUrl() && method === 'GET' && isRunReadConflict(status, body);
      if (declaredConflict) allowedResources.set(url, (allowedResources.get(url) ?? 0) + 1);
      else errors.push(`${method} ${url} → ${status}: ${body ?? '<unreadable body>'}`);
    }
    for (const { text, url } of consoleErrors) {
      const remaining = allowedResources.get(url) ?? 0;
      // Pair one Chromium resource diagnostic with one body-validated response at that URL.
      if (remaining > 0 && text === 'Failed to load resource: the server responded with a status of 409 (Conflict)') {
        allowedResources.set(url, remaining - 1);
      } else errors.push(text);
    }
    return [...captureErrors, ...errors, ...pageErrors];
  };
}

/** The negotiated run read can refuse to publish a snapshot that changed during the read. */
export function isRunReadConflict(status: number, body: string | null): boolean {
  if (status !== 409 || body === null) return false;
  try {
    const value: unknown = JSON.parse(body);
    return value !== null && typeof value === 'object'
      && 'code' in value && value.code === 'conflict'
      && 'error' in value && value.error === 'conversation runtime changed while reading'
      && Object.keys(value).length === 2;
  } catch {
    return false;
  }
}
