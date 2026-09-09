# Next Android client

This records the original 0.1 launcher-only slice. For the current bundled
frontend, see [the 0.2 design](../docs/mobile-bundled-frontend.md).

Outcome: install a Tauri 2 Android APK, enter a Neige Calm server's HTTPS origin,
and use its existing `/next/` application, session cookies, and event WebSocket.
The committed profile has no preselected server or HTTP exceptions. A private
build profile can configure explicit local or USB-forwarded HTTP origins.
The server must be reachable from the phone. The Linux server, workers, and Codex
runtime remain on the server. This is an online client, not an offline frontend.

## Boundaries

- A packaged connection page accepts an HTTPS origin (or its `/next/` URL),
  remembers only that origin locally, and navigates the top-level WebView to Next.
- Login happens on the server's origin; no credentials are copied to the launcher.
- No native commands, plugins, or remote IPC capabilities are granted.
- Keep TLS certificate validation. Deny cleartext by default; generate narrow
  Android host exceptions from `server-profile.json`. Launcher validation also
  checks the exact configured origin/port. Android host rules cover all ports.
- Do not embed Next in an iframe or modify its API, CORS, cookies, or event transport.
- Keep the mobile crate in its own Cargo workspace so server build dependencies
  and release artifacts are unaffected.
- Reopening the app presents the connection page, including the saved server,
  so a typo or an unavailable server does not permanently trap the user.

## Acceptance

1. Validate input and reject credentials, non-HTTPS URLs outside the local
   profile, queries, fragments, unexpected paths, and the launcher's own origin.
2. Browser-check the production connection page at phone size: validation,
   navigation to `/next/`, saved address, and usable layout.
3. Generate the Android project using Tauri; compile an ARM64 debug APK and check
   its package identity, signing, permissions, and embedded assets.
4. Review the full mobile change independently through two channels.
5. Report device-only verification separately: actual server login, reconnect,
   keyboard, Android back navigation, and attachment flows need a device/server.

This initial build is for direct installation/testing. A store release requires
the owner's release-signing key and Android device acceptance.
