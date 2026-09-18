package io.neigecalm.next

import android.content.Context
import java.net.URI
import org.json.JSONObject

/** A navigation hint, never identity or cached page content. */
internal data class SavedEntry(val origin: String, val route: String)
internal class ResumeEntry(context: Context) {
  private val preferences = context.getSharedPreferences("workspace-resume", Context.MODE_PRIVATE)
  fun read(profiles: ConnectionProfiles): SavedEntry? = runCatching {
    val raw = preferences.getString("entry", null) ?: return null
    require(raw.length <= 2048)
    val value = JSONObject(raw)
    require(value.getInt("schemaVersion") == 1 && value.getString("profileId") == profiles.profileId())
    require(value.getLong("configRevision") == profiles.revision())
    val origin = value.getString("origin")
    require(profiles.read().candidates().any { it.origin == origin })
    val route = value.getString("route")
    require(route == safeRoute(URI(origin + route)))
    SavedEntry(origin, route)
  }.getOrNull()
  fun clear() { check(preferences.edit().remove("entry").commit()) { "无法清除上次页面，请重试" } }
  fun remember(profiles: ConnectionProfiles, origin: BundledOrigin, url: String) {
    val uri = runCatching { URI(url) }.getOrNull() ?: return
    if (!origin.matches(uri)) return
    val route = safeRoute(uri) ?: return
    val value = JSONObject().put("schemaVersion", 1).put("profileId", profiles.profileId())
      .put("configRevision", profiles.revision()).put("origin", origin.value).put("route", route)
    preferences.edit().putString("entry", value.toString()).apply()
  }
  companion object {
    fun safeRoute(uri: URI): String? {
      if (uri.rawUserInfo != null || uri.rawFragment != null) return null
      val path = uri.rawPath ?: return null
      if (path.contains('%') || path.contains('\\') || path.split('/').any { it == "." || it == ".." }) return null
      if (path == "/next" || path == "/next/") return "/next/"
      if (path.matches(Regex("/next/area/[A-Za-z0-9_-]{1,128}/new"))) return "/next/"
      if (path.matches(Regex("/next/track/[A-Za-z0-9_-]{1,128}"))) {
        val values = linkedMapOf<String, String>()
        for (pair in (uri.rawQuery ?: "").split('&')) {
          val parts = pair.split('=')
          if (parts.size == 2 && !values.containsKey(parts[0])) values[parts[0]] = parts[1]
        }
        val query = mutableListOf<String>()
        values["card"]?.takeIf { it.matches(Regex("[A-Za-z0-9_-]{1,128}")) }?.let { query.add("card=$it") }
        values["panel"]?.takeIf { it in listOf("outline", "cards", "tasks", "conversations") }?.let { query.add("panel=$it") }
        values["from"]?.takeIf { it in listOf("pages", "area") }?.let { query.add("from=$it") }
        return path + if (query.isEmpty()) "" else "?" + query.joinToString("&")
      }
      if (path == "/next/recipes" || path.matches(Regex("/next/settings(?:/(general|network|plugins|appearance|about))?"))) return path
      return null
    }
  }
}
