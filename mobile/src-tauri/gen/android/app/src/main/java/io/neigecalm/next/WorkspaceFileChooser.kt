package io.neigecalm.next

import android.app.Activity
import android.content.Intent
import android.content.pm.PackageManager
import android.net.Uri
import android.os.Process
import android.webkit.ValueCallback
import android.webkit.WebChromeClient
import android.webkit.WebView
import androidx.activity.ComponentActivity
import androidx.activity.result.contract.ActivityResultContracts
import java.net.URI
import java.util.UUID

internal fun interface FileChooserLauncher {
  fun launch(intent: Intent, result: (Int, Intent?) -> Unit): () -> Unit
}

/** Only document selection; no camera, JS interface, Tauri client or IPC. */
internal class WorkspaceFileChooser(
  private val activity: Activity,
  private val view: WebView,
  private val origin: BundledOrigin,
  private val launch: FileChooserLauncher,
) : WebChromeClient() {
  private val owner = FileSelectionOwner<Array<Uri>> {
    val uri = runCatching { URI(view.url ?: "") }.getOrNull()
    uri != null && origin.matches(uri) && (uri.rawPath == "/next" || uri.rawPath.startsWith("/next/"))
  }
  override fun onShowFileChooser(webView: WebView, callback: ValueCallback<Array<Uri>>, params: FileChooserParams): Boolean {
    if (webView !== view || params.mode !in listOf(FileChooserParams.MODE_OPEN, FileChooserParams.MODE_OPEN_MULTIPLE)) {
      callback.onReceiveValue(null); return true
    }
    val types = params.acceptTypes.flatMap { it.split(',') }.map { it.trim().lowercase() }
      .filter { it.matches(Regex("[a-z0-9.+-]+/(?:[a-z0-9.+-]+|\\*)")) }.distinct().take(16)
    val intent = Intent(Intent.ACTION_OPEN_DOCUMENT).addCategory(Intent.CATEGORY_OPENABLE)
      .setType(types.singleOrNull() ?: "*/*")
      .putExtra(Intent.EXTRA_ALLOW_MULTIPLE, params.mode == FileChooserParams.MODE_OPEN_MULTIPLE)
    if (types.size > 1) intent.putExtra(Intent.EXTRA_MIME_TYPES, types.toTypedArray())
    owner.begin({ callback.onReceiveValue(it) }) { done ->
      launch.launch(intent) { code, data ->
        val selected = if (code == Activity.RESULT_OK && data != null &&
          data.flags and Intent.FLAG_GRANT_READ_URI_PERMISSION != 0) FileChooserParams.parseResult(code, data) else null
        val accepted = selected?.takeIf { uris -> uris.isNotEmpty() && uris.size <= 64 && uris.all { uri ->
          uri.scheme == "content" && !uri.authority.isNullOrEmpty() &&
            activity.packageManager.resolveContentProvider(uri.authority!!, 0)?.applicationInfo?.uid?.let { it != Process.myUid() } == true &&
            activity.checkUriPermission(uri, Process.myPid(), Process.myUid(), Intent.FLAG_GRANT_READ_URI_PERMISSION) == PackageManager.PERMISSION_GRANTED
        } }
        done(accepted)
      }
    }
    return true
  }
  fun documentChanged() = owner.cancel()
  fun dispose() = owner.dispose()

  companion object {
    fun launcher(activity: ComponentActivity) = FileChooserLauncher { intent, result ->
      // Register each request separately: an old picker result cannot be
      // redirected to a new callback after disposal or Activity replacement.
      val launcher = activity.activityResultRegistry.register("neige-scan-file-" + UUID.randomUUID(), ActivityResultContracts.StartActivityForResult()) {
        result(it.resultCode, it.data)
      }
      try { launcher.launch(intent) } catch (error: Exception) { launcher.unregister(); throw error }
      val unregister: () -> Unit = { launcher.unregister() }
      unregister
    }
  }
}
