package io.neigecalm.next

import android.content.Context
import androidx.webkit.WebViewCompat
import androidx.webkit.WebViewFeature

internal object BundledWebViewSupport {
  fun available(context: Context): Boolean {
    val major = WebViewCompat.getCurrentWebViewPackage(context)?.versionName?.substringBefore('.')?.toIntOrNull()
    return major != null && major >= 111 && WebViewFeature.isFeatureSupported(WebViewFeature.GET_WEB_VIEW_CLIENT)
  }
}
