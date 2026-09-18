package io.neigecalm.next

import android.util.Base64
import org.json.JSONObject
import java.security.SecureRandom

/** A native-only, one-document value. It is never returned to launcher JS. */
internal class ScanDocument(bootstrap: JSONObject) {
  private var pending: String? = bootstrap.toString().also { encoded ->
    require(encoded.length <= 2048)
    require(bootstrap.length() == 7 && bootstrap.has("generation") && bootstrap.has("origin") &&
      bootstrap.has("enrollmentId") && bootstrap.has("attemptId") && bootstrap.has("attemptSecret") &&
      bootstrap.has("pairTicket") && bootstrap.has("deadline"))
  }
  @Synchronized fun takeScript(nonce: String): String? {
    val value = pending ?: return null
    pending = null
    val encoded = Base64.encodeToString(value.toByteArray(Charsets.UTF_8), Base64.NO_WRAP)
    return "<script nonce=\"$nonce\">Object.defineProperty(window,'__NEIGE_SCAN__',{value:JSON.parse(atob('$encoded')),configurable:true});</script>"
  }
  @Synchronized fun clear() { pending = null }
  companion object {
    fun nonce(): String = Base64.encodeToString(ByteArray(24).also { SecureRandom().nextBytes(it) }, Base64.NO_WRAP)
  }
}
