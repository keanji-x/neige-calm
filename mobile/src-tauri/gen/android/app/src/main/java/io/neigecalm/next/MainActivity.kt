package io.neigecalm.next

import android.os.Bundle
import android.view.View
import android.view.ViewGroup
import android.webkit.WebView
import androidx.activity.OnBackPressedCallback
import androidx.activity.enableEdgeToEdge
import androidx.appcompat.app.AlertDialog

class MainActivity : TauriActivity() {
  private var connectionBack: OnBackPressedCallback? = null
  // MainActivity owns history and the workspace-to-configuration transition.
  // Wry registers its callback later, so its built-in handler must stay disabled.
  override val handleBackNavigation: Boolean = false

  override fun onCreate(savedInstanceState: Bundle?) {
    enableEdgeToEdge()
    super.onCreate(savedInstanceState)
    connectionBack = object : OnBackPressedCallback(true) {
      override fun handleOnBackPressed() {
        val view = findWebView(findViewById(android.R.id.content))
        val uri = runCatching { java.net.URI(view?.url ?: "") }.getOrNull()
        // Keep the native runtime alive when leaving the root screen.
        if (view == null || uri?.host == "tauri.localhost") { moveTaskToBack(true); return }
        if (view.canGoBack()) view.goBack()
        else if (uri?.scheme in listOf("http", "https")) view.loadUrl("http://tauri.localhost/")
        else moveTaskToBack(true)
      }
    }
    installConnectionBackHandler()
    if (!BundledWebViewSupport.available(this)) {
      AlertDialog.Builder(this).setTitle("请更新系统网页组件")
        .setMessage("请更新 Android System WebView 或系统浏览器后，再打开 Neige App。")
        .setPositiveButton("关闭") { _, _ -> finish() }.setCancelable(false).show()
    }
  }
  internal fun installConnectionBackHandler() {
    // Tauri's core AppPlugin also registers Back after Activity creation.
    // Re-register once the real launcher invokes its bridge, after plugin load.
    connectionBack?.let { callback ->
      callback.remove()
      onBackPressedDispatcher.addCallback(this, callback)
    }
  }
  private fun findWebView(view: View): WebView? {
    if (view is WebView) return view
    if (view is ViewGroup) for (index in 0 until view.childCount) findWebView(view.getChildAt(index))?.let { return it }
    return null
  }

}
