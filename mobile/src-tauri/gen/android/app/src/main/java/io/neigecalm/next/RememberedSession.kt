package io.neigecalm.next

import android.webkit.CookieManager

/** Persist only the existing workspace cookie, inside WebView's private store.
 * This is a resume hint, never an authentication verdict: the server still
 * checks expiry/revocation on every request. No cookie is exposed to JavaScript.
 */
internal object RememberedSession {
  private var remembered: String? = null
  private fun token(): String? = CookieManager.getInstance().getCookie(P2PConnection.ORIGIN)
    ?.split(';')?.map { it.trim() }?.firstOrNull { it.startsWith("calm-session=") }
    ?.substringAfter('=')?.takeIf { it.matches(Regex("[A-Za-z0-9_-]{16,256}")) }

  fun hasCookie(): Boolean = token() != null

  fun persist(done: (Boolean) -> Unit = {}) {
    val value = token() ?: run { done(false); return }
    if (remembered == value) { done(true); return }
    CookieManager.getInstance().setCookie(P2PConnection.ORIGIN,
      "calm-session=$value; Path=/; Secure; HttpOnly; SameSite=Strict; Max-Age=2592000") { saved ->
      if (saved) { remembered = value; CookieManager.getInstance().flush() }
      done(saved)
    }
  }
}
