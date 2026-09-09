# Neige Calm Next for Android

A Tauri 2 Android client for the existing Neige Calm **Next** app at `/next/`.
The APK contains a connection page. Enter the HTTPS address of your Neige Calm
server and sign in using the existing Next login. The server supplies the actual
Next UI; the Linux server, workers, and Codex runtime continue running remotely.
No server credentials are bundled into the APK.

Requires an ARM64 phone running Android 7.0/API 24 or later, an up-to-date Android
System WebView, and a server reachable from that phone. This is an online client.
The committed profile has no default server and denies cleartext HTTP. Trusted
HTTPS is the default; self-signed TLS certificates remain rejected.

`server-profile.json` is the default input for `npm run configure`, which generates
`www/server-config.js` and Android network-security XML. A build can explicitly
select a private profile using `npm run configure -- /absolute/path/profile.local.json`
before `npm run android:apk`. Keep real deployment addresses in an ignored
`*.local.json` file or outside this checkout. Do not commit the private profile or
its generated outputs. After the local build, run `npm run configure` to restore
the shareable default configuration.

A profile must contain `defaultServer` (an empty string or server origin) and
`httpOrigins` (an array of exact IPv4 HTTP origins). For USB development, an
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
Set `JAVA_HOME`, `ANDROID_HOME`, and `NDK_HOME` to those installations.

From `mobile/`:

```sh
rustup target add aarch64-linux-android
npm ci
npm run configure
env -u NEIGE_CODEX_BIN RUSTC_WRAPPER= CARGO_BUILD_JOBS=6 npm run android:apk
```

The APK is emitted below `src-tauri/gen/android/app/build/outputs/apk/`.
The debug package ID is `io.neigecalm.next.debug`; its display name is Neige Calm.
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

## Verification

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
cleartext traffic is restricted to the profile's explicit hosts; app backup is
disabled, including for the test APK. The
connection page's CSP is local to that page; the loaded server applies its own
policy. Next API calls, cookies, and event WebSockets remain on the server's
origin. This client does not change server CORS, authentication, or the `fe/`
architecture. See [DESIGN.md](DESIGN.md) for acceptance boundaries.
