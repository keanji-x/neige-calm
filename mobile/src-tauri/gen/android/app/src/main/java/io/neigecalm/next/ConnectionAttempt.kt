package io.neigecalm.next

internal data class ConnectionFailure(val mode: String, val message: String)
internal data class ConnectionOutcome(val route: ConnectionRoute?, val failures: List<ConnectionFailure>)

/** One bounded pass: explicit Tailnet choice is pinned, otherwise IP-first. Never sends session credentials. */
internal object ConnectionAttempt {
  class Cancellation {
    @Volatile private var cancelled = false
    fun check() { if (cancelled || Thread.currentThread().isInterrupted) throw java.util.concurrent.CancellationException("连接已取消") }
    fun cancel() { cancelled = true }
  }
  fun firstAvailable(settings: ConnectionSettings, tailnetOrigin: String? = null, check: (ConnectionRoute) -> Unit): ConnectionOutcome {
    val candidates = if (tailnetOrigin == null) settings.candidates() else {
      require(tailnetOrigin.isNotEmpty() && settings.tailscaleEnabled && tailnetOrigin == settings.tailnetOrigin) { "所选工作区已改变，请重新选择" }
      listOf(ConnectionRoute("tailscale", tailnetOrigin))
    }
    val failures = mutableListOf<ConnectionFailure>()
    for (candidate in candidates) {
      try { check(candidate); return ConnectionOutcome(candidate, failures) }
      catch (error: java.util.concurrent.CancellationException) { throw error }
      catch (error: InterruptedException) { Thread.currentThread().interrupt(); throw error }
      catch (error: Exception) { failures.add(ConnectionFailure(candidate.mode, error.message ?: "连接不可用")) }
    }
    return ConnectionOutcome(null, failures)
  }

}
