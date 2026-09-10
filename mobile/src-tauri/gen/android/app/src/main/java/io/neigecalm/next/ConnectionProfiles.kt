package io.neigecalm.next

import android.content.Context
import java.net.InetAddress
import java.net.URI

internal data class ConnectionRoute(val mode: String, val origin: String)
internal data class ConnectionSettings(val mode: String, val ipOrigin: String, val tailscaleEnabled: Boolean) {
  fun candidates(): List<ConnectionRoute> = buildList {
    if (ipOrigin.isNotEmpty()) add(ConnectionRoute("ip", ipOrigin))
    if (tailscaleEnabled) add(ConnectionRoute("tailscale", P2PConnection.ORIGIN))
  }
}

internal class ConnectionProfiles(context: Context) {
  private val legacyTailIdentity = java.io.File(context.noBackupFilesDir, "p2p-node").isDirectory
  private val preferences = context.getSharedPreferences("connection-profiles", Context.MODE_PRIVATE)
  fun read() = ConnectionSettings(preferences.getString("mode", "tailscale")!!,
    preferences.getString("ip-origin", "")!!, preferences.getBoolean("tailscale-enabled", legacyTailIdentity))
  fun save(mode: String, ipOrigin: String, tailscaleEnabled: Boolean): ConnectionSettings {
    require(mode in listOf("ip", "tailscale")) { "请选择 IP 或 Tailscale" }
    val origin = if (ipOrigin.isBlank()) "" else parseDirect(ipOrigin.trim()).value
    check(preferences.edit().putString("mode", mode).putString("ip-origin", origin)
      .putBoolean("tailscale-enabled", tailscaleEnabled).commit()) { "保存连接配置失败，请重试" }
    return ConnectionSettings(mode, origin, tailscaleEnabled)
  }
  companion object {
    fun literalHttpHost(host: String): Boolean {
      val name = host.removeSurrounding("[", "]")
      if (name.contains('%')) return false
      if (name.contains(':')) {
        val address = runCatching { InetAddress.getByName(name) }.getOrNull() ?: return false
        return !address.isLoopbackAddress && !address.isAnyLocalAddress && !address.isLinkLocalAddress && !address.isMulticastAddress
      }
      val parts = name.split('.')
      if (parts.size != 4 || parts.any { it.isEmpty() || (it.length > 1 && it[0] == '0') || !it.all(Char::isDigit) }) return false
      val n = parts.map { it.toIntOrNull() ?: return false }
      if (n.any { it !in 0..255 }) return false
      return n[0] in 1..223 && n[0] != 127 && !(n[0] == 169 && n[1] == 254)
    }
    fun parseDirect(value: String): BundledOrigin {
      val uri = try { URI(if (value.contains("://")) value else "http://$value") }
        catch (_: Exception) { throw IllegalArgumentException("请输入合法的服务器地址") }
      require(uri.rawPath.isNullOrEmpty() || uri.rawPath in listOf("/", "/next", "/next/")) { "请输入服务器地址，不要包含其他路径" }
      require(uri.rawUserInfo == null && uri.rawQuery == null && uri.rawFragment == null) { "地址不能包含账号、查询参数或锚点" }
      val root = URI(uri.scheme, null, uri.host, uri.port, null, null, null).toASCIIString()
      return BundledOrigin.parse(root) { literalHttpHost(it) }
    }
  }
}
