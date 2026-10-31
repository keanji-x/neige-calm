import type { Page } from '@playwright/test';

/** The allowed URL is supplied after the create response identifies the new conversation. */
export function captureFirstMessageErrors(page: Page, runUrl: () => string | undefined): () => Promise<string[]> {
  const pageErrors: string[] = [];
  const consoleErrors: { text: string; url: string }[] = [];
  const failures: Promise<{ url: string; method: string; status: number; body: string | null }>[] = [];
  page.on('console', message => {
    if (message.type() === 'error') consoleErrors.push({ text: message.text(), url: message.location().url });
  });
  page.on('pageerror', error => pageErrors.push(error.message));
  page.on('response', response => {
    if (response.status() < 400) return;
    // Start reading immediately, before query invalidation can dispose of the response.
    // An unavailable body is evidence we cannot classify, never permission to ignore it.
    failures.push(response.text().then(body => ({
      url: response.url(), method: response.request().method(), status: response.status(), body,
    }), () => ({
      url: response.url(), method: response.request().method(), status: response.status(), body: null,
    })));
  });

  return async () => {
    const errors: string[] = [];
    const allowedResources = new Map<string, number>();
    for (const failure of failures) {
      const { url, method, status, body } = await failure;
      let declaredConflict = false;
      if (url === runUrl() && method === 'GET' && status === 409 && body !== null) {
        try {
          const value: unknown = JSON.parse(body);
          declaredConflict = value !== null && typeof value === 'object'
            && 'code' in value && value.code === 'conflict'
            && 'error' in value && value.error === 'conversation runtime changed while reading'
            && Object.keys(value).length === 2;
        } catch { /* Malformed bodies stay unexpected HTTP failures. */ }
      }
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
    return [...errors, ...pageErrors];
  };
}
