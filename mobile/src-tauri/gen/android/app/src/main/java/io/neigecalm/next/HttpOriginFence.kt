package io.neigecalm.next

import java.net.URI

/** Android permits cleartext for explicitly configured IP addresses; WebViews may
 * send it only to the exact currently selected origin. */
internal object HttpOriginFence {
  fun permits(url: String, selected: BundledOrigin?): Boolean {
    val uri = runCatching { URI(url) }.getOrNull() ?: return false
    if (uri.scheme != "http") return true
    if (uri.host == "tauri.localhost" && uri.port in listOf(-1, 80) && uri.rawUserInfo == null) return true
    return selected?.matches(uri) == true && ConnectionProfiles.literalHttpHost(selected.host)
  }
}
