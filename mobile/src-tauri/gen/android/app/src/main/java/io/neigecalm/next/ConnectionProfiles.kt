package io.neigecalm.next

import android.content.Context
import java.net.InetAddress
import java.net.URI

internal data class ConnectionRoute(val mode: String, val origin: String)
internal data class ConnectionSettings(val mode: String, val ipOrigin: String, val tailscaleEnabled: Boolean, val tailnetOrigin: String) {
  fun candidates(): List<ConnectionRoute> = buildList {
    if (ipOrigin.isNotEmpty()) add(ConnectionRoute("ip", ipOrigin))
    if (tailscaleEnabled && tailnetOrigin.isNotEmpty()) add(ConnectionRoute("tailscale", tailnetOrigin))
  }
}

internal class ConnectionProfiles(private val preferences: android.content.SharedPreferences) {
  constructor(context: Context) : this(context.getSharedPreferences("connection-profiles", Context.MODE_PRIVATE))
  init {
    if (!preferences.contains("schema-version")) check(preferences.edit().putInt("schema-version", 1)
      .putString("profile-id", java.util.UUID.randomUUID().toString()).putLong("config-revision", 1).commit()) { "无法保存连接配置" }
  }
  private data class Identity(val id: String, val revision: Long)
  private fun identity(): Identity {
    require(preferences.getInt("schema-version", 0) == 1) { "连接配置版本无效，请重新配置" }
    val id = requireNotNull(preferences.getString("profile-id", null))
    require(java.util.UUID.fromString(id).toString() == id) { "连接配置身份无效，请重新配置" }
    val revision = preferences.getLong("config-revision", 0)
    require(revision > 0) { "连接配置版本无效，请重新配置" }
    return Identity(id, revision)
  }
  fun profileId(): String = identity().id
  fun revision(): Long = identity().revision
  fun needsLegacyConfirmation(): Boolean = read().let { it.tailscaleEnabled && it.tailnetOrigin == P2PConnection.ORIGIN }
  fun read(): ConnectionSettings { identity(); tailnetOrigins(); return readSettings() }
  private fun readSettings(): ConnectionSettings {
    val result = ConnectionSettings(preferences.getString("mode", "tailscale")!!,
      preferences.getString("ip-origin", "")!!, preferences.getBoolean("tailscale-enabled", false), selectedTailnet())
    require(result.mode in listOf("ip", "tailscale"))
    if (result.ipOrigin.isNotEmpty()) require(parseDirect(result.ipOrigin).value == result.ipOrigin)
    if (result.tailnetOrigin.isNotEmpty()) require(BundledOrigin.parse(result.tailnetOrigin) { false }.value == result.tailnetOrigin)
    return result
  }
  private fun selectedTailnet(): String = (preferences.getString("tailnet-origin", null)
    ?: if (preferences.getBoolean("tailscale-enabled", false)) P2PConnection.ORIGIN else "").also {
      if (it.isNotEmpty()) require(BundledOrigin.parse(it) { false }.value == it)
    }
  fun tailnetOrigins(): List<String> {
    val encoded = preferences.getString("tailnet-origins", "[]")!!
    val array = org.json.JSONArray(encoded)
    require(array.length() <= 8) { "工作区配置无效" }
    return (0 until array.length()).map { (array.get(it) as String).also { raw -> require(BundledOrigin.parse(raw) { false }.value == raw) } }.distinct()
  }
  // Only explicit save/verified-scan/reset may repair storage. Passive reads
  // remain strict; preserve each independently valid configuration field.
  private fun repairableDirect(): String = runCatching {
    val value = preferences.getString("ip-origin", "")!!
    if (value.isNotEmpty()) require(parseDirect(value).value == value)
    value
  }.getOrDefault("")
  fun selectTailnet(origin: String): ConnectionSettings {
    require(BundledOrigin.parse(origin) { false }.value == origin)
    val known = runCatching { tailnetOrigins() }.getOrDefault(emptyList())
    require(origin in known || known.size < 8) { "已保存的工作区达到上限" }
    return saveSelected("tailscale", repairableDirect(), true, origin, (known + origin).distinct())
  }
  fun disableTailnet(): ConnectionSettings = saveSelected("tailscale", repairableDirect(), false, "", emptyList())
  fun selectSavedTailnet(origin: String): ConnectionSettings {
    require(origin in tailnetOrigins()) { "请扫描这个工作区的二维码" }
    val old = read()
    return saveSelected("tailscale", old.ipOrigin, true, origin, tailnetOrigins())
  }
  fun directBinding(origin: String): String = runCatching {
    val raw = preferences.getString("direct-binding", "")!!
    require(raw.length <= 8192 && org.json.JSONObject(raw).getString("origin") == origin)
    raw
  }.getOrDefault("")
  fun confirmDirectBinding(origin: String, binding: String) {
    val old = read()
    require(old.ipOrigin == origin && binding.length <= 8192 && org.json.JSONObject(binding).getString("origin") == origin) { "连接配置已改变" }
    saveSelected(old.mode, old.ipOrigin, old.tailscaleEnabled, old.tailnetOrigin, tailnetOrigins(), binding)
  }
  fun save(mode: String, ipOrigin: String, tailscaleEnabled: Boolean): ConnectionSettings {
    val selected = runCatching { selectedTailnet() }.getOrDefault("")
    val known = runCatching { tailnetOrigins() }.getOrDefault(emptyList())
    return saveSelected(mode, ipOrigin, tailscaleEnabled && selected.isNotEmpty(), selected, known)
  }
  private fun saveSelected(mode: String, ipOrigin: String, tailscaleEnabled: Boolean, tailnetOrigin: String, known: List<String>, confirmedDirectBinding: String? = null): ConnectionSettings {
    require(mode in listOf("ip", "tailscale")) { "请选择 IP 或 Tailscale" }
    val origin = if (ipOrigin.isBlank()) "" else parseDirect(ipOrigin.trim()).value
    val settings = ConnectionSettings(mode, origin, tailscaleEnabled, tailnetOrigin)
    // Read untrusted metadata once. An explicit save repairs invalid identity
    // atomically with the settings, so an old ResumeEntry cannot become valid.
    val previous = runCatching { identity() }.getOrNull()
    val retainedBinding = if (runCatching { readSettings().ipOrigin }.getOrNull() == origin) directBinding(origin) else ""
    val binding = confirmedDirectBinding ?: retainedBinding
    val changed = runCatching { read() }.getOrNull() != settings || binding != retainedBinding
    val next = if (previous == null || (changed && previous.revision == Long.MAX_VALUE))
      Identity(java.util.UUID.randomUUID().toString(), 1)
    else Identity(previous.id, previous.revision + if (changed) 1 else 0)
    check(preferences.edit().putInt("schema-version", 1)
      .putString("profile-id", next.id).putLong("config-revision", next.revision)
      .putString("mode", mode).putString("ip-origin", origin)
      .putString("direct-binding", binding)
      .putBoolean("tailscale-enabled", tailscaleEnabled).putString("tailnet-origin", tailnetOrigin)
      .putString("tailnet-origins", org.json.JSONArray(known).toString()).commit()) { "保存连接配置失败，请重试" }
    return settings
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
