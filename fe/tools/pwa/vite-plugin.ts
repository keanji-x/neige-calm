import type { Plugin } from 'vite';

// Links the install manifest served from the public directory
// (web/public/manifest.webmanifest) into index.html, under the resolved base.
export function pwaManifestLink(): Plugin {
  let base = '/';
  return {
    name: 'neige-pwa-manifest-link',
    configResolved(config) {
      base = config.base;
    },
    transformIndexHtml: () => [
      { tag: 'link', attrs: { rel: 'manifest', href: `${base}manifest.webmanifest` }, injectTo: 'head' },
    ],
  };
}
