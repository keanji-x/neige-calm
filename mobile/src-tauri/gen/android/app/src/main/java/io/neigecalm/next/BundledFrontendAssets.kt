package io.neigecalm.next

import android.content.res.AssetManager
import android.webkit.WebResourceRequest
import android.webkit.WebResourceResponse
import org.json.JSONObject
import java.io.ByteArrayInputStream
import java.util.Collections
import java.util.concurrent.atomic.AtomicReference

internal data class BundledAsset(val path: String, val mime: String, val size: Long)

internal class BundledFrontendAssets(private val assets: AssetManager, val origin: AtomicReference<BundledOrigin?>) {
  private val files: Map<String, BundledAsset>
  private val policy: BundledSelectionPolicy

  init {
    val manifest = assets.open("neige-next/manifest.json").bufferedReader().use { JSONObject(it.readText()) }
    require(manifest.getInt("version") == 1) { "Unsupported frontend manifest" }
    require(manifest.getString("sourceRevision").matches(Regex("[0-9a-f]{40}"))) { "Invalid frontend source revision" }
    manifest.getBoolean("sourceDirty")
    require(manifest.getInt("webCompatVersion") > 0) { "Invalid frontend compatibility version" }
    val entries = manifest.getJSONArray("files")
    val result = linkedMapOf<String, BundledAsset>()
    for (index in 0 until entries.length()) {
      val entry = entries.getJSONObject(index)
      val path = entry.getString("path")
      val mime = entry.getString("mime")
      val size = entry.getLong("size")
      require(size in 0..33554432 && entry.getString("sha256").matches(Regex("[0-9a-f]{64}"))) { "Invalid bundled asset metadata" }
      require(mime.matches(Regex("[a-z-]+/[a-z0-9.+-]+"))) { "Invalid bundled asset MIME type" }
      require(result.put(path, BundledAsset(path, mime, size)) == null) { "Duplicate bundled asset path" }
    }
    files = Collections.unmodifiableMap(result)
    policy = BundledSelectionPolicy(files.keys)
    require(files.getValue("index.html").mime == "text/html") { "Invalid bundled entry document" }
  }

  fun response(request: WebResourceRequest): WebResourceResponse? {
    val bound = origin.get()
    return when (val selected = policy.select(bound, request.url.toString(), request.method, request.isForMainFrame)) {
    BundledSelection.Network -> null
    is BundledSelection.Error -> error(selected.status)
    is BundledSelection.File -> {
      val file = files.getValue(selected.path)
      try {
        val headers = mutableMapOf("Cache-Control" to "no-store", "X-Content-Type-Options" to "nosniff", "Content-Length" to file.size.toString())
        if (file.mime == "text/html") {
          val websocket = checkNotNull(bound).value.replaceFirst("https://", "wss://").replaceFirst("http://", "ws://")
          headers["Content-Security-Policy"] = "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; " +
            "img-src 'self' data: blob: https:; connect-src 'self' " + websocket + "; font-src 'self' data:; " +
            "media-src 'self' data: blob:; worker-src 'self' blob:; object-src 'none'; base-uri 'none'; " +
            "frame-ancestors 'none'; form-action 'self'"
        }
        WebResourceResponse(file.mime, if (file.mime.startsWith("text/")) "UTF-8" else null,
          200, "OK", headers,
          assets.open("neige-next/" + file.path))
      } catch (_: java.io.IOException) { error(500) }
    }
    }
  }

  private fun error(status: Int): WebResourceResponse = WebResourceResponse(
    "text/plain", "UTF-8", status, "Bundled resource unavailable",
    mapOf("Cache-Control" to "no-store", "X-Content-Type-Options" to "nosniff"),
    ByteArrayInputStream("安装包资源不可用，请更新 Neige App。".toByteArray(Charsets.UTF_8)),
  )
}
