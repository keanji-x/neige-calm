# Direct hostname binding implementation

This implements the already-approved direct HTTPS requirement in
`1712-tailnet-mobile-recovery.md` section 7. It does not change scan enrollment,
Tailnet peer binding, session authority, or the general IP-first fallback policy.

- The existing Android ConnectionProfiles owner persists one non-secret binding
  for its direct origin: schema version, exact origin, and the full canonical
  sorted DNS address set. Literal ordinary LAN IPs are already explicit numeric
  configuration and need no DNS confirmation. Reserved destinations are denied.
- Only the launcher's explicit save-and-connect IP action requests confirmation.
  The existing NativeOperation admission is reserved before dispatch; the Go
  check uses its cancellation context. It validates every A/AAAA answer, chooses
  one numeric address, and performs the credential-free version probe through
  that address while retaining the original hostname for TLS and HTTP.
- Android persists the successful binding only after its existing generation
  check. A changed binding increments the existing profile revision, retiring
  old resume hints. Passive settings reads/saves, automatic retries, Tailnet
  selection, and bind/resume never confirm a new address set. Changing the direct
  origin drops its old binding; malformed metadata cannot grant networking.
- Proxy installation parses configuration and binds loopback only; it never
  waits for DNS. Cold local assets can paint while offline, including for a
  pre-upgrade hostname profile with no binding. Such a profile's network requests
  remain denied until the user returns to save-and-connect IP explicitly.
- Before every new HTTP proxy request and CONNECT tunnel, Go resolves the full
  set with a bounded context and requires exact set equality to the saved
  binding. Reordering/duplicates do not change a set; additions, removals,
  replacements, empty responses and mixed reserved answers are not accepted.
  There is no fallback to another answer or DNS lookup at the actual dial.
  HTTP connection reuse does not skip this per-request check.
- Both paths dial the validated numeric IP and original port. Reverse HTTP TLS
  verifies the original hostname; CONNECT passes TLS unchanged to WebView.
  Exact-origin/port/protocol checks remain before credentials are forwarded.
  Probe redirects are refused. Replacing/stopping a proxy cancels pending DNS
  and dials, closes owned connections and rejects a late dial before registration.

The previous independent Java HttpURLConnection probe is removed, so it cannot
resolve a hostname outside this boundary. No system DNS configuration, Tailscale
account, general network stack, second session owner or alternative credential
mechanism is added. Android/device execution and final package provenance remain
separate acceptance gates, not consequences of local fixture success.
