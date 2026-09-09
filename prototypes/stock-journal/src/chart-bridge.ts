import type { MarketRead } from './market-adapter';

/** No request can choose a Track other than the active route and its own frame. */
export function attachChartBridge(read: (id: string) => MarketRead | undefined) {
  function frameFor(id: string): HTMLIFrameElement | undefined {
    const route = /^\/next\/track\/([^/]+)$/.exec(window.location.pathname);
    if (!route || decodeURIComponent(route[1]) !== id) return;
    return [...document.querySelectorAll<HTMLIFrameElement>('iframe[title="组合概览图表"]')].find(frame => {
      const url = new URL(frame.src, window.location.href);
      return url.origin === location.origin && url.pathname === '/next/portfolio-demo.html' && url.searchParams.get('track') === id;
    });
  }
  const publish = (id: string, result: MarketRead) => {
    const frame = frameFor(id);
    // '*' is required for this specific opaque-origin iframe. No auth token,
    // cookie or API capability is included in the validated snapshot.
    frame?.contentWindow?.postMessage({ type: 'neige:portfolio-snapshot', trackId: id, result }, '*');
  };
  const onMessage = (event: MessageEvent) => {
    const data = event.data;
    if (!data || data.type !== 'neige:portfolio-ready' || typeof data.trackId !== 'string') return;
    const frame = frameFor(data.trackId);
    if (!frame || event.source !== frame.contentWindow) return;
    const result = read(data.trackId);
    if (result) publish(data.trackId, result);
  };
  window.addEventListener('message', onMessage);
  return { publish, dispose: () => window.removeEventListener('message', onMessage) };
}
