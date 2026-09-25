# Neige Calm frontend

This is an independent npm project; it deliberately is not a root workspace member.

`make dev` builds this frontend and serves it through the Docker stack at
`http://localhost:<printed-port>/next/`. The legacy frontend remains available
under `/calm/` during the cutover.

For HMR, run `make fe-dev` from the repository root, then open
`http://localhost:5180/next/`. The `/next/` prefix matches the built mount path
and also applies to LAN previews.

## Install in Chrome

Open `/next/` in Chrome using HTTPS, or HTTP on `localhost` / `127.0.0.1`.
Use Chrome's address-bar install icon or **Cast, save, and share → Install page
as app** (the menu label varies by Chrome version), then confirm the installation.
Launch **Neige Calm** from the system application launcher to open it in its own
window. Existing links under `/next/` remain within the app's scope. Installation
is available on the sign-in page; the app uses the same session as the website in
that Chrome profile.

Plain HTTP on a LAN IP is not a secure context: use HTTPS for installation from
another machine. If Chrome does not offer installation, check the address,
whether the app is already installed, and DevTools → Application → Manifest.
The app still needs a running server and network connection. This installation
support adds no service worker or offline cache; reload to receive a deployed
frontend update. Chrome manages updates to installed names and icons separately.

The icons are generated from `web/src/ui/brand/neige-mark.svg`. To regenerate them:

```sh
npx playwright install --with-deps chromium
node tools/pwa/generate-icons.mjs
```

Run `npx playwright test --config tools/pwa/playwright.config.ts` for a production
build check covering Chrome manifest diagnostics, deep links, installation and
standalone launch. It uses a temporary browser profile and home directory.
Playwright pages receive a routed signed-out API response; Chrome's background
install fetch is not routed, so the preview server proxies `/api` to a closed
local port (`127.0.0.1:9`). No kernel, agent runtime or backend is required or
reached. CI runs it in the `fe-browser` job; the same tests also run against the
dev server as part of `npm run e2e`, where the background fetch goes to the
configured `FE_API_PROXY_TARGET`.

`@astryxdesign/core` is pinned exactly. Astryx shipped 12 releases in 5.5 weeks, 67% with breaking changes and no codemod, so upgrades must be reviewed as dedicated work.

The module-state lint rule defaults both `new` and top-level calls to rejection. Its pure-factory exceptions are source-and-export-specific. To add one, document why its returned object graph is immutable, register that exact import/API in `tools/architecture/no-module-runtime-state.mjs`, and add a passing regression fixture plus a mutation that makes only that fixture fail. TypeScript `as const` is not runtime immutability; freeze static arrays as `Object.freeze([... ] as const)`.
