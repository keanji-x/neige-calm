package io.neigecalm.next

import android.app.Activity
import android.view.View
import android.view.ViewGroup
import android.webkit.WebResourceRequest
import android.webkit.WebSettings
import android.webkit.WebView
import android.webkit.WebViewClient
import java.net.URI
import java.util.concurrent.atomic.AtomicReference

/** Document lifetime only: RecoverySession remains the sole session/retry owner.
 * This WebView has no JavaScript interfaces, Tauri client, or native IPC scripts.
 */
internal class ScanWorkspace(
  private val activity: Activity,
  private val launcher: WebView,
  private val origin: BundledOrigin,
  private val document: ScanDocument,
  private val visited: (String) -> Unit,
  private val leave: () -> Unit,
) {
  val view = WebView(activity)
  private var closed = false
  private val selected = AtomicReference<BundledOrigin?>(origin)
  init {
    view.settings.apply {
      javaScriptEnabled = true; domStorageEnabled = true
      allowFileAccess = false; allowContentAccess = false
      mixedContentMode = WebSettings.MIXED_CONTENT_NEVER_ALLOW
      setSupportMultipleWindows(false); javaScriptCanOpenWindowsAutomatically = false
      userAgentString = launcher.settings.userAgentString
    }
    val native = object : WebViewClient() {
      override fun shouldOverrideUrlLoading(webView: WebView, request: WebResourceRequest): Boolean = navigation(request.url.toString(), request.isForMainFrame)
      @Suppress("DEPRECATION")
      override fun shouldOverrideUrlLoading(webView: WebView, url: String): Boolean = navigation(url, true)
      private fun navigation(raw: String, main: Boolean): Boolean {
        if (!main) return true
        val uri = runCatching { URI(raw) }.getOrNull() ?: return true
        if (uri.host == "tauri.localhost") { leave(); return true }
        return !origin.matches(uri) || !(uri.rawPath == "/next" || uri.rawPath.startsWith("/next/"))
      }
    }
    view.webViewClient = BundledWebViewClient(native, BundledFrontendAssets(activity.assets, selected, document)) { url ->
      if (!closed) visited(url)
    }
    val root = activity.findViewById<ViewGroup>(android.R.id.content)
    launcher.visibility = View.GONE
    root.addView(view, ViewGroup.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.MATCH_PARENT))
    view.requestFocus()
  }
  fun open(route: String) { check(!closed); view.loadUrl(origin.value + route) }
  fun pause() { if (!closed) view.onPause() }
  fun resume() { if (!closed) view.onResume() }
  fun back() { if (view.canGoBack()) view.goBack() else leave() }
  fun destroy() {
    if (closed) return
    // Caller closes this generation's native sockets before destroying it.
    closed = true; document.clear()
    selected.set(null)
    view.stopLoading(); (view.parent as? ViewGroup)?.removeView(view)
    view.removeAllViews(); view.destroy()
    launcher.visibility = View.VISIBLE; launcher.requestFocus()
  }
}
