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

## Resolver completeness and intent continuity

The resolver explicitly requests IPv4 and IPv6 independently. Successful family
results complete a family; empty/no-record results need the platform's absence
proof. Temporary errors, timeouts and other failures abort the whole lookup.
A combined `LookupNetIP("ip")` is not evidence of
completeness: both Go and cgo can return one family while another failed.
The scoped resolver enables Go StrictErrors but preserves platform selection
and DNS routing, including Android's cgo/private-DNS path. Completeness does
not depend on StrictErrors being honored by cgo, and Android is not forced onto
Go's resolv.conf discovery or a new DNS server/fallback.

Android Bionic may collapse a proxy failure into `EAI_NODATA`, which Go reports
as `DNSError.IsNotFound`. Single-family getaddrinfo is therefore insufficient
evidence for absence there. An Android-only absence check dynamically resolves
the supported API29 `android_res_nquery`, `android_res_nresult`, and
`android_res_cancel` symbols from libandroid. Querying uses NETWORK_UNSPECIFIED,
class IN, the same hostname/family, and flags 0: platform default/private DNS,
cache, and routing remain in charge, with no public server or network override.
Successful nonempty getaddrinfo results still follow the existing path.

The pinned `golang.org/x/net/dns/dnsmessage` parser validates the complete packet.
Both native and packet rcodes must be NOERROR, with a matching question, no
truncation, no positive family answer, and an SOA-backed negative response for
the name (or the end of a complete acyclic CNAME chain). Referrals, bare empty
packets, malformed responses, mismatched questions, NXDOMAIN inconsistencies,
and vendor errors cannot establish absence. This check never adds raw-query
addresses to the binding; disagreement requires retry instead of fallback.

The five-second parent deadline also bounds absence checks. A canceled start
cleans up a late descriptor. Once admitted, one owner polls in bounded steps;
result reads are nonblocking and consume/close the descriptor, while every
other exit calls android_res_cancel exactly once. No symbol is hard-linked,
so minSDK26 loading remains unchanged. On API26-28 or any vendor missing the
complete API, ambiguous hostname absence fails closed with an explicit message
to use a literal IP, Tailnet, or Android 10+ with supported DNS APIs. This is a
narrow compatibility limitation, not full DNS support on API26-28. Tailnet,
literal IPs, and hostname resolutions with nonempty answers for both families
are unchanged. The parent explicitly approved this disposition for R3.

Source references: NDK r29 `android/multinetwork.h` and the official networking
reference at https://developer.android.com/ndk/reference/group/networking;
Bionic's proxy error mapping at
https://android.googlesource.com/platform/bionic/+/master/libc/dns/net/getaddrinfo.c.
Host ABI fixtures compile the actual adapter and model the collapsed Bionic
error, supported no-data/rcode results, missing symbols, vendor failures,
cancellation and descriptor ownership. They are not Android device evidence.

A saved-workspace selection is durable intent in ConnectionProfiles, not a
one-click argument. It survives failed attempts, reconnect, passive saves,
disable/re-enrollment and process reopening. An older Tailnet-mode record whose
selected origin is in its saved-workspace list is conservatively retained as
that choice. Only an explicit mode choice clears it; general mode still has
the existing IP-first policy. A pinned but unavailable/disabled target never
implies consent to use the retained direct server.

Launcher connection requests carry a per-attempt identifier in the existing
native Pending owner, through checking and binding. Editing an unsaved IP draft
sends a matching-owner cancellation, without saving the draft. A late old
cancellation cannot revoke a newer Pending owner. Cancellation uses the existing
native operation ticket and generation checks; no separate ownership registry,
session owner, or general authentication refactor is introduced.

The instrumentation regression holds a real JNI version proof at a local HTTP
peer, edits the actual launcher, crosses a read-only native command barrier, then
releases the peer. It checks unchanged binding/revision/resume state and that a
stale cancellation leaves a newer proof able to persist. This fixture requires
fresh-package Android execution; host browser mocks do not substitute for it.
