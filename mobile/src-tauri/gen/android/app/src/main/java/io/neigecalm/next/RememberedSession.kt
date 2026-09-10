package io.neigecalm.next

import android.webkit.CookieManager

/** Persist only the existing workspace cookie, inside WebView's private store.
 * This is a resume hint, never an authentication verdict: the server still
 * checks expiry/revocation on every request. No cookie is exposed to JavaScript.
 */
internal object RememberedSession {
  private val remembered = mutableMapOf<String, String>()
  private fun token(origin: String): String? = CookieManager.getInstance().getCookie(origin)
    ?.split(';')?.map { it.trim() }?.firstOrNull { it.startsWith("calm-session=") }
    ?.substringAfter('=')?.takeIf { it.matches(Regex("[A-Za-z0-9_-]{16,256}")) }

  fun hasCookie(origin: String = P2PConnection.ORIGIN): Boolean = token(origin) != null

  fun persist(origin: String = P2PConnection.ORIGIN, done: (Boolean) -> Unit = {}) {
    val value = token(origin) ?: run { done(false); return }
    if (remembered[origin] == value) { done(true); return }
    val secure = if (origin.startsWith("https://")) "; Secure" else ""
    CookieManager.getInstance().setCookie(origin,
      "calm-session=$value; Path=/$secure; HttpOnly; SameSite=Strict; Max-Age=2592000") { saved ->
      if (saved) { remembered[origin] = value; CookieManager.getInstance().flush() }
      done(saved)
    }
  }
}
