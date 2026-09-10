package io.neigecalm.next

import org.json.JSONObject
import java.net.HttpURLConnection
import java.net.Proxy
import java.net.URL
import java.io.ByteArrayOutputStream
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit

internal data class ConnectionFailure(val mode: String, val message: String)
internal data class ConnectionOutcome(val route: ConnectionRoute?, val failures: List<ConnectionFailure>)

/** One bounded pass, preserving IP-first order. Never sends session credentials. */
internal object ConnectionAttempt {
  private val deadlines = Executors.newSingleThreadScheduledExecutor()
  fun firstAvailable(settings: ConnectionSettings, check: (ConnectionRoute) -> Unit): ConnectionOutcome {
    val failures = mutableListOf<ConnectionFailure>()
    for (candidate in settings.candidates()) {
      try { check(candidate); return ConnectionOutcome(candidate, failures) }
      catch (error: InterruptedException) { Thread.currentThread().interrupt(); throw error }
      catch (error: Exception) { failures.add(ConnectionFailure(candidate.mode, error.message ?: "连接不可用")) }
    }
    return ConnectionOutcome(null, failures)
  }

  fun checkDirect(origin: String) {
    val validated = ConnectionProfiles.parseDirect(origin)
    val connection = URL(validated.value + "/api/version").openConnection(Proxy.NO_PROXY) as HttpURLConnection
    connection.connectTimeout = 3000
    connection.readTimeout = 3000
    connection.instanceFollowRedirects = false
    connection.useCaches = false
    connection.setRequestProperty("Accept", "application/json")
    val timeout = deadlines.schedule({ connection.disconnect() }, 5, TimeUnit.SECONDS)
    try {
      check(connection.responseCode == 200) { "服务器未响应 Neige 接口" }
      val data = ByteArrayOutputStream()
      connection.inputStream.use { stream ->
        val buffer = ByteArray(4096)
        while (true) {
          val count = stream.read(buffer)
          if (count < 0) break
          check(data.size() + count <= 65536) { "服务器响应异常" }
          data.write(buffer, 0, count)
        }
      }
      val version = JSONObject(data.toString("UTF-8"))
      check(version.getInt("webCompatVersion") > 0 && version.getString("apiVersion").isNotEmpty()
        && version.getString("kernelVersion").isNotEmpty()) { "这个地址不是 Neige 服务器" }
    } finally { timeout.cancel(false); connection.disconnect() }
  }
}
