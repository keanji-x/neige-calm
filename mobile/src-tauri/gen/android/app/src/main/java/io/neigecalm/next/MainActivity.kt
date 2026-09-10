package io.neigecalm.next

import android.os.Bundle
import android.view.View
import android.view.ViewGroup
import android.webkit.WebView
import androidx.activity.OnBackPressedCallback
import androidx.activity.enableEdgeToEdge
import androidx.appcompat.app.AlertDialog

class MainActivity : TauriActivity() {
  // Tauri disables Wry's history handling by default; Next is a browser app.
  override val handleBackNavigation: Boolean = true

  override fun onCreate(savedInstanceState: Bundle?) {
    enableEdgeToEdge()
    super.onCreate(savedInstanceState)
    onBackPressedDispatcher.addCallback(this, object : OnBackPressedCallback(true) {
      override fun handleOnBackPressed() {
        val view = findWebView(findViewById(android.R.id.content))
        val uri = runCatching { java.net.URI(view?.url ?: "") }.getOrNull()
        if (view == null || uri?.host == "tauri.localhost") { finish(); return }
        if (view.canGoBack()) view.goBack()
        else if (uri?.scheme in listOf("http", "https")) view.loadUrl("http://tauri.localhost/")
        else finish()
      }
    })
    if (!BundledWebViewSupport.available(this)) {
      AlertDialog.Builder(this).setTitle("请更新系统网页组件")
        .setMessage("请更新 Android System WebView 或系统浏览器后，再打开 Neige App。")
        .setPositiveButton("关闭") { _, _ -> finish() }.setCancelable(false).show()
    }
  }
  private fun findWebView(view: View): WebView? {
    if (view is WebView) return view
    if (view is ViewGroup) for (index in 0 until view.childCount) findWebView(view.getChildAt(index))?.let { return it }
    return null
  }

}
