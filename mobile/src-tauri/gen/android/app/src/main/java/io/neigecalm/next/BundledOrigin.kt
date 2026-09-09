package io.neigecalm.next

import java.net.URI
import java.net.InetAddress
import java.util.Locale

internal data class BundledOrigin(val scheme: String, val host: String, val port: Int) {
  val value: String
    get() = URI(scheme, null, host, if (port == defaultPort(scheme)) -1 else port, null, null, null).toASCIIString()

  fun matches(uri: URI): Boolean = try {
    fromRequest(uri) == this
  } catch (_: IllegalArgumentException) {
    false
  }

  companion object {
    private fun defaultPort(scheme: String): Int = if (scheme == "https") 443 else 80

    private fun reservedHost(host: String): Boolean {
      val name = host.trimEnd('.').removeSurrounding("[", "]")
      if (name == "localhost" || name.endsWith(".localhost") || name.contains('%')) return true
      if (name.contains(':')) {
        val address = try { InetAddress.getByName(name) } catch (_: Exception) { return true }
        return address.isLoopbackAddress || address.isAnyLocalAddress
      }
      if (name.matches(Regex("[0-9.]+"))) {
        val parts = name.split('.')
        if (parts.size != 4 || parts.any { it.length > 1 && it.startsWith('0') }) return true
        val octets = parts.map { it.toIntOrNull() ?: return true }
        return octets.any { it !in 0..255 } || octets.first() == 127 || octets.all { it == 0 }
      }
      return name.split('.').all { it.matches(Regex("(?:[0-9]+|0x[0-9a-f]+)")) }
    }

    private fun fromRequest(uri: URI): BundledOrigin {
      val scheme = requireNotNull(uri.scheme) { "Missing server scheme" }.lowercase(Locale.ROOT)
      val host = requireNotNull(uri.host) { "Missing server hostname" }.lowercase(Locale.ROOT)
      require((scheme == "https" || scheme == "http") && host.isNotEmpty()) { "Invalid server origin" }
      require(uri.rawUserInfo == null && (uri.port == -1 || uri.port in 1..65535)) { "Invalid server authority" }
      return BundledOrigin(scheme, host, if (uri.port == -1) defaultPort(scheme) else uri.port)
    }

    fun parse(value: String, allowCleartext: (String) -> Boolean): BundledOrigin {
      val uri = try { URI(value) } catch (error: Exception) { throw IllegalArgumentException("Invalid server origin", error) }
      require(uri.rawPath.isNullOrEmpty() || uri.rawPath == "/") { "Expected a server origin, not a path" }
      require(uri.rawQuery == null && uri.rawFragment == null) { "Expected a server origin, not a query or fragment" }
      val origin = fromRequest(uri)
      val configuredHttp = origin.scheme == "http" && allowCleartext(origin.host)
      val hostname = origin.host.trimEnd('.')
      val launcher = hostname == "localhost" || hostname.endsWith(".localhost")
      require(!reservedHost(origin.host) || (configuredHttp && !launcher)) { "The server cannot use a privileged or unconfigured local origin" }
      require(origin.scheme == "https" || configuredHttp) { "This installation requires HTTPS" }
      return origin
    }
  }
}

internal sealed class BundledSelection {
  data class File(val path: String) : BundledSelection()
  data class Error(val status: Int) : BundledSelection()
  data object Network : BundledSelection()
}

internal class BundledSelectionPolicy(paths: Set<String>) {
  private val paths = paths.toSet()
  init {
    require(paths.contains("index.html")) { "Missing bundled entry document" }
    require(paths.all { it.matches(Regex("(?:index\\.html|assets/[A-Za-z0-9_.-]+)")) && !it.split('/').contains("..") }) { "Invalid bundled asset path" }
  }

  fun select(origin: BundledOrigin?, url: String, method: String, mainFrame: Boolean): BundledSelection {
    val uri = try { URI(url) } catch (_: Exception) { return BundledSelection.Network }
    if (origin == null || !origin.matches(uri)) return BundledSelection.Network
    val path = uri.rawPath ?: return BundledSelection.Network
    if (path != "/next" && !path.startsWith("/next/")) return BundledSelection.Network
    if (method != "GET") return BundledSelection.Error(405)
    if (path.contains('%') || path.contains('\\') || path.split('/').any { it == "." || it == ".." }) return BundledSelection.Error(400)
    val relative = path.removePrefix("/next/")
    if (paths.contains(relative)) return BundledSelection.File(relative)
    if (path == "/next/assets" || path.startsWith("/next/assets/") || path.substringAfterLast('/').contains('.')) return BundledSelection.Error(404)
    return if (mainFrame) BundledSelection.File("index.html") else BundledSelection.Error(404)
  }
}
