# Android frontend bundled with the application

## Outcome

The installed Android application reads the existing Next frontend's HTML,
JavaScript, styles, icons and fonts from its APK. Only authentication, workspace
data, live events and explicitly requested files use the paired server. A cold
start must not download the frontend bundle through Funnel.

The first Android slice packaged the launcher and scanner only. Measurement on
the deployed public route found a 2,781,329-byte JavaScript response without
compression; a direct transfer received 835,584 bytes in 45 seconds. A mobile
browser still had an empty root after entering Next, with no JavaScript error.
Bundling the frontend addresses that transfer directly.

## Boundaries

- Build the same `fe/` implementation for the APK; do not fork features or data
  models. Copy only build outputs and a generated asset manifest into Android
  assets. Never package user data, deployment addresses or credentials.
- Keep the confirmed server's HTTPS origin in the WebView. An Android request
  interceptor supplies the bundled `/next/` document and manifest-listed assets
  for that exact origin. Normal HTTP requests and WebSockets still reach the
  server, retaining the current Secure/HttpOnly pairing cookie and same-origin
  behavior. No global CORS relaxation or native HTTP credential bridge is needed.
- The packaged launcher alone may bind a server origin through an explicitly
  permissioned inline Tauri plugin. The existing launcher may remember the
  address; native authority is established anew for its WebView before navigation.
  Reject privileged launcher origins, userinfo, query/fragment components,
  invalid ports and disallowed cleartext hosts. Remote pages cannot rebind it or
  obtain camera/native permissions.
- Match method, origin and path before serving an asset. Use an immutable
  generated path-to-asset mapping; reject traversal and unknown static assets.
  Do not silently fetch a missing frontend bundle from the server. API paths,
  files and other origins retain the original WebView client's behavior.
- The bundled document has an App-owned CSP: local scripts, the bound server's
  API/WebSocket, no framing or objects, and the media/image sources required by
  the shared UI. Remote bootstrap and API responses retain server policy.
- Preserve Tauri/Wry navigation, initialization, error and SSL callbacks through
  delegation. Do not patch framework-generated or dependency source files and
  never bypass certificate verification in the delivered application.
- Install the wrapper during the explicit bind command, after Wry has installed
  its client. Call that original client's interceptor first and always delegate
  `onPageStarted`: Wry's IPC object retains the original instance and reads its
  `currentUrl` to determine native authority. Wrapping synchronously in
  `onWebViewCreate` would be overwritten by Wry initialization.
- Keep binding and session lifetimes distinct: remembering a server origin does
  not grant access. Expired/revoked sessions still require the existing pairing
  flow. This change does not introduce persistent device credentials.
- An Android build that is too old for the server shows an explicit App-update
  message. Reloading must not promise to update an APK-bundled frontend. The web
  build retains its existing browser-refresh behavior.
- Require a server whose advertised web compatibility can support the bundled
  client, and distinguish an App update from a server update. Show connection
  progress and a re-pairing entry on expired sessions. Manual owner login remains
  available for servers that support it.
- Android API 24 remains installable; the frontend requires Chromium/WebView 111
  or newer. An older component receives a native upgrade message. Retrieve the
  original client through the feature-checked AndroidX accessor.

## Acceptance

1. Generate and verify the complete asset manifest from a real frontend build;
   confirm packaged bytes, MIME types, source revision and compatibility version.
2. Exercise the production Android interceptor: document/deep-link and known
   assets are local; API/WS behavior is preserved; wrong origins, traversal and
   unknown static paths cannot select local files or gain native privileges.
3. Launch the actual activity in a dedicated emulator against a real temporary
   calm-server behind a TLS proxy that refuses every `/next/` request. Assert zero
   frontend downloads, successful authenticated API/WS traffic, deep-link/offline
   behavior, remote IPC denial after navigation, and rejection of untrusted TLS.
   Only the separate `instrumented` variant trusts the ephemeral test CA; normal
   debug/release APKs must not contain it.
4. Verify the real Next UI, paired login and an incompatible-server response in
   a browser. Verify that waiting/offline states are visible instead of blank.
5. Build and verify the ARM64 APK; retain the existing scanner/cancellation tests.
   Use a dedicated CI Android emulator for native execution when local hardware
   acceleration or a physical device is unavailable. Report those limits exactly.
6. Run focused tests, mutation-verify authority assertions, obtain two independent
   fresh complete reviews, and merge only with required CI green.

Server-side static compression remains a separate web-delivery improvement; it
does not substitute for the Android no-bundle-download acceptance check.

## Ownership decision

For issue #1604, the orchestrator approves the narrow `fe/vite.config.ts` change needed to build
the existing frontend in Android mode, including its explicit WebView target and
bundled-client constant. No API, event or global style contract is changed.
