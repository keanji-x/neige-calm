package io.neigecalm.next

import android.os.Bundle
import androidx.activity.enableEdgeToEdge
import androidx.appcompat.app.AlertDialog

class MainActivity : TauriActivity() {
  // Tauri disables Wry's history handling by default; Next is a browser app.
  override val handleBackNavigation: Boolean = true

  override fun onCreate(savedInstanceState: Bundle?) {
    enableEdgeToEdge()
    super.onCreate(savedInstanceState)
    if (!BundledWebViewSupport.available(this)) {
      AlertDialog.Builder(this).setTitle("请更新系统网页组件")
        .setMessage("请更新 Android System WebView 或系统浏览器后，再打开 Neige App。")
        .setPositiveButton("关闭") { _, _ -> finish() }.setCancelable(false).show()
    }
  }
}
