import type { Plugin } from 'vite';

// Links the web build's install manifest (web/public/manifest.webmanifest).
// The Android bundle ships neither the manifest nor its icons, so vite.config.ts
// registers this plugin only for the web platform.
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
