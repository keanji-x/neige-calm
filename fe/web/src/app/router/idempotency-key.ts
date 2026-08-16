/**
 * A fresh `Idempotency-Key` for a conversation draft.
 *
 * # Why this is its own module
 *
 * Because of the counter below, which is module state and must be: it has to
 * outlive every mount. `useConversationPanel` is a *page* hook — the cove page,
 * the wave page and Today each instantiate one, and navigating away and back
 * remounts it — so a `useRef` counter restarts at zero on a return to the same
 * cove and mints the key that cove already used. `architecture/no-module-runtime-state`
 * forbids that state, rightly, for whole feature files; the allowlist in
 * `tools/architecture/allowlists.mjs` carries this file by name so the
 * exception covers these twenty lines and nothing else.
 *
 * # Why not `crypto.randomUUID()`
 *
 * Because it does not exist here. `randomUUID()` is `[SecureContext]` in the
 * Web Crypto IDL, exactly like `crypto.subtle`, and this app is served over
 * plain http on a LAN — `docker/nginx.conf` listens on 8080 with no TLS, the
 * server binds a plain `TcpListener`, and the reader opens
 * `http://<lan-ip>:<port>/calm/` from another machine, which is neither https
 * nor localhost. `window.isSecureContext` is false there, so the call that
 * used to mint this key threw and `+` → send was dead in the one place the app
 * actually runs. (This is the same fact that forces the hand-written digest in
 * `core/domain/sha256.ts`; the mistake was applying it to only one of the two
 * APIs it governs.)
 *
 * `crypto.getRandomValues` is the one member of `Crypto` that carries no
 * `[SecureContext]` qualifier, so it *is* there — and it is used when present.
 * `Math.random` is the fallback for an environment that somehow has neither.
 *
 * # Why a non-cryptographic fallback is allowed
 *
 * The key needs to be **unique**, not unguessable. It scopes one client's
 * retries: the card id is `sha256(cove_id, key)`, so a collision with somebody
 * else's key hits an existing card and answers 409 — a refusal, not a
 * disclosure. Nor is guessing a key a way in: `GET /api/coves/{id}/conversations`
 * already returns *every* conversation on the cove to any authenticated caller
 * (`crates/calm-server/src/routes/cove_conversations.rs`, `list_cove_conversations`
 * filters by cove only), so an attacker who could use a guessed key could
 * equally well have just read the list. Nothing is authenticated by this value.
 *
 * The shape is a v4 UUID because the server takes any non-blank ASCII string
 * and a familiar shape is easier to read in a log.
 *
 * # What the counter guarantees, and what it does not
 *
 * *Unique* is a real requirement and `Math.random` does not supply it: nothing
 * in the spec forbids an implementation that returns the same number twice —
 * or always — and with one, every retry would re-send the key that was just
 * refused, at the same derived card, forever. So the fallback stamps the first
 * four bytes with `fallbackMints`, which only ever goes up.
 *
 * Guaranteed: **two fallback mints in this page never return the same key**,
 * whatever `Math.random` does. (Four bytes, so strictly the first 2³² mints —
 * not a number reachable by pressing "try again".)
 *
 * Not guaranteed, and not needed: nothing across a reload, another tab, or
 * another device. The key exists to tell *this* client's attempts apart, and a
 * collision with a stranger's key lands on an existing card and is answered by
 * a 409 — the refusal described above.
 */
let fallbackMints = 0;

export function mintIdempotencyKey(): string {
  const bytes = new Uint8Array(16);
  const source: Crypto | undefined = globalThis.crypto;
  if (typeof source?.getRandomValues === 'function') source.getRandomValues(bytes);
  else {
    for (let index = 0; index < bytes.length; index += 1) bytes[index] = Math.floor(Math.random() * 256);
    /* Assigned, not mixed in: XOR-ing the counter over random bytes would leave
       two mints free to land on the same value, and "distinct" is the whole
       point. Bytes 6 and 8 carry the UUID version and variant, so the counter
       lives in 0..3, where nothing overwrites it. */
    fallbackMints += 1;
    bytes[0] = (fallbackMints >>> 24) & 0xff;
    bytes[1] = (fallbackMints >>> 16) & 0xff;
    bytes[2] = (fallbackMints >>> 8) & 0xff;
    bytes[3] = fallbackMints & 0xff;
  }
  bytes[6] = (bytes[6] & 0x0f) | 0x40;
  bytes[8] = (bytes[8] & 0x3f) | 0x80;
  const hex = [...bytes].map((byte) => byte.toString(16).padStart(2, '0')).join('');
  return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`;
}
