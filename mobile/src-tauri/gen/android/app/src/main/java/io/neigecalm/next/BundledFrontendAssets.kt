package io.neigecalm.next

import android.content.res.AssetManager
import android.webkit.WebResourceRequest
import android.webkit.WebResourceResponse
import org.json.JSONObject
import java.io.ByteArrayInputStream
import java.util.Collections
import java.util.concurrent.atomic.AtomicReference

internal data class BundledAsset(val path: String, val mime: String, val size: Long)

internal class BundledFrontendAssets(private val assets: AssetManager, val origin: AtomicReference<BundledOrigin?>, private val scan: ScanDocument? = null) {
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
        val headers = mutableMapOf("Cache-Control" to "no-store", "X-Content-Type-Options" to "nosniff")
        val stream = if (file.mime == "text/html") {
          val websocket = checkNotNull(bound).value.replaceFirst("https://", "wss://").replaceFirst("http://", "ws://")
          val nonce = ScanDocument.nonce()
          headers["Content-Security-Policy"] = "default-src 'self'; script-src 'nonce-$nonce' 'strict-dynamic'; style-src 'self' 'unsafe-inline'; " +
            "img-src 'self' data: blob:; connect-src " + bound.value + " " + websocket + "; font-src 'self' data:; " +
            "media-src 'self' data: blob:; worker-src " + bound.value + "/next/assets/ blob:; object-src 'none'; base-uri 'none'; " +
            "frame-src 'none'; frame-ancestors 'none'; form-action 'self', " +
            // Intersect nonce/strict-dynamic with the APK-only path. Trust
            // propagation must not allow an API response to become a script.
            "script-src 'nonce-$nonce' " + bound.value + "/next/assets/"
          val html = assets.open("neige-next/" + file.path).bufferedReader().use { it.readText() }
          val boot = if (request.isForMainFrame) scan?.takeScript(nonce) ?: "" else ""
          val document = html.replace("<script ", "<script nonce=\"$nonce\" ")
            .replace("<head>", "<head>$boot")
          ByteArrayInputStream(document.toByteArray(Charsets.UTF_8))
        } else assets.open("neige-next/" + file.path)
        WebResourceResponse(file.mime, if (file.mime.startsWith("text/")) "UTF-8" else null,
          200, "OK", headers, stream)
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
