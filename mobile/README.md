# Neige Calm Next for Android

A Tauri 2 Android client for the existing Neige Calm **Next** app at `/next/`.
Use **扫码连接** to scan a QR created in web Settings → Network → Mobile connection.
Confirm the server hostname and compare the verification code on both screens;
the owner approves the phone in the web page. The phone then signs in over HTTPS
without a Tailscale app. See [server setup and scope](../docs/mobile-qr-pairing.md).

The APK contains the connection page and the full existing Next frontend. Its
HTML, scripts, styles and icons load from Android assets under the confirmed
server's origin. Authentication, API calls, event WebSockets and requested files
continue to use the remote server. No server credentials or workspace data are
bundled into the APK. See [the bundled frontend design](../docs/mobile-bundled-frontend.md).

Requires an ARM64 phone running Android 8.0/API 26 or later, an up-to-date Android
System WebView (Chromium 111 or newer), and a server reachable from that phone.
Older components show a native upgrade message. This is an online data client.
The committed profile has no default server and denies cleartext HTTP. Trusted
HTTPS is the default; self-signed TLS certificates remain rejected.

`server-profile.json` is the default input for `npm run configure`, which generates
`www/server-config.js` and Android network-security XML. A build can explicitly
select a private profile using `npm run configure -- /absolute/path/profile.local.json`
before `npm run android:apk`. Keep real deployment addresses in an ignored
`*.local.json` file or outside this checkout. Do not commit the private profile or
its generated outputs. After the local build, run `npm run configure` to restore
the shareable default configuration.

A profile must contain `defaultServer` (an empty string or server origin),
`httpOrigins` (an array of exact IPv4 HTTP origins), and `allowConfiguredHttp`
(a boolean enabling the native exact-origin HTTP fence for runtime IP setup). For USB development, an
explicit `http://127.0.0.1:4140` origin can be paired with
`adb reverse tcp:4140 tcp:4140`; without forwarding, loopback points to the phone.
For LAN use, select the host's address reachable from the phone. Android scopes
HTTP exceptions by exact host (all ports); the launcher additionally requires the
exact configured origin/port. No global cleartext allowance is used.

The connection page can remember the server origin. Force-close and reopen the
app to return to that page and change the server. Saved login sessions are managed
by the server and Android WebView, separately from the remembered server address.

## Build an installable test APK

Install the [Tauri Android prerequisites](https://v2.tauri.app/start/prerequisites/#android):
JDK 17+, Android SDK command-line tools/platform tools, Android SDK platform 36,
build-tools 35.0.0 (used by the generated Gradle plugin), Android NDK, Node.js, and the repository-pinned Rust toolchain.
Install Go using the version pinned in `p2p-native/go.mod`. The standard Gradle
packaging tasks build the userspace networking library for each requested ABI.
Set `JAVA_HOME`, `ANDROID_HOME`, and `NDK_HOME` to those installations.

From `mobile/`:

```sh
rustup target add aarch64-linux-android
npm ci
npm --prefix ../fe ci
npm run configure
env -u NEIGE_CODEX_BIN RUSTC_WRAPPER= CARGO_BUILD_JOBS=6 npm run android:apk
```

The APK is emitted below `src-tauri/gen/android/app/build/outputs/apk/`.
The build first compiles `fe/` in Android mode and generates a manifest for all
bundled resources. Verify the actual APK with
`npm run verify:apk -- /absolute/path/to/app.apk`. App updates ship new UI code;
reloading a bundled frontend does not download an update from the server.
The debug package ID is `io.neigecalm.next.p2ptrial`; its display name is Neige Calm.
Install it directly on a phone or use `adb install -r /absolute/path/to/app.apk`.
The debug signing key is generated locally by Android's build tools. Builds from
another computer may require uninstalling the old test app first, which removes
its local data. This is a test build, not a signed store release.

The generated Android project is checked in. Do not run `android:init` as a
routine build step: initialization can overwrite Android customizations. If
regenerating it deliberately, preserve the cleartext/backup restrictions, Gradle
worker limit, and MainActivity behavior. App icons come from
`../fe/web/src/ui/brand/neige-mark.svg`; the bundled copy is `www/neige-mark.svg`.
Regenerate icon assets with `npm run tauri -- icon www/neige-mark.svg`.
The generator also emits iOS, macOS, and Windows icons; those unused platform
assets are ignored. Keep the Android resources and PNG icons referenced by the
Tauri configuration in version control.

## Verification

The `Android client` workflow runs the profile tests, Chromium connection tests,
and an ARM64 APK build on an ephemeral GitHub-hosted runner. Native emulator jobs
also launch the real activity against a temporary real backend, refuse frontend
network requests, and test native authority after navigation. Successful runs
provide a debug APK and connection-page screenshot as a seven-day artifact.
CI builds use the committed Tailscale endpoint and no prefilled IP connection.

```sh
npm test
npx playwright install chromium
npm run test:browser
cargo fmt --manifest-path src-tauri/Cargo.toml --check
```

Browser checks load the shipped connection-page files with the configured CSP.
They check phone layout, invalid input, navigation, remembering/removing the
server, and storage failure. The destination response is intercepted: these
checks do **not** establish that a real server login or Android WebView works.
The screenshot is written to `artifacts/connection-page.png`.

Before distribution, verify on a physical phone: server login, navigation/back,
background/resume and event reconnect, keyboard resizing, attachments, and report
rendering. External link/download handling follows Android WebView behavior and
needs device acceptance. Store distribution additionally requires the owner's
release signing configuration. Never commit a signing key or password.

## Security and scope

Remote pages receive no Tauri native capabilities or custom commands. Android
camera access is granted only to the packaged launcher, when the user starts a
scan. Only that launcher may bind the origin used by the local asset interceptor.
Permission refusal/cancellation preserves manual connection. Android
cleartext traffic is restricted by the native fixed-origin proxy to the configured
IP server; app backup is
disabled, including for the test APK. The
connection page and bundled Next document have their own CSP; remote pairing
and API responses keep the server's policy. Next API calls, cookies, and event WebSockets remain on the server's
origin. This client does not change server CORS or authentication. Expired
sessions show a re-pairing entry, and incompatible versions distinguish updating
the App from updating the server.

The `instrumented` build type is exclusively for CI. It trusts a short-lived test
CA generated by `tests/native/backend.mjs`, has a separate application ID, and is
never a distribution artifact. Normal debug/release builds retain their original
trust policy; APK verification rejects the test CA in a distributable build.

## Signed release APK

`npm run android:release` builds an ARM64 release with R8 enabled and debugging
disabled. Sign the resulting unsigned APK with your own persistent key using
`apksigner`; keep the key and password outside this repository. Run
`npm run verify:apk -- /absolute/path/to/signed.apk`, `apksigner verify`, and
`zipalign -c -P 16 4` on the final artifact. The release ID is `io.neigecalm.next`;
it installs separately from `io.neigecalm.next.p2ptrial` and starts with independent
credentials. Subsequent release updates must use the same signing key.

The launcher saves IP and Tailscale configuration, tries IP first, and falls back
once to Tailscale if configured. Neither route being available returns to setup.
WebView cookies follow browser host/domain scope: different hosts have separate
sessions, while different ports on the same host share cookies. Only configure
trusted services; the connection selector is not an isolation boundary between
services on the same host.
