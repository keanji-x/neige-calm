package io.neigecalm.next

import android.content.Context
import org.json.JSONObject
import java.io.File
import java.util.concurrent.Executors
import java.util.concurrent.CompletableFuture
import java.util.concurrent.TimeUnit

/** One userspace node per app process; node identity survives in noBackupFilesDir. */
internal object P2PConnection {
  const val ORIGIN = "https://pivot-neige.tail328551.ts.net:10000"
  private val worker = Executors.newSingleThreadExecutor()
  private var started = false
  private var failure: Throwable? = null

  @Synchronized fun start(context: Context) {
    if (started) return
    started = true
    val app = context.applicationContext
    worker.execute {
      runCatching {
        checked(NativeP2P.configure(AndroidNetworkSnapshot.read()))
        checked(NativeP2P.start(File(app.noBackupFilesDir, "p2p-node").absolutePath))
      }.onFailure { failure = it }
    }
  }

  fun execute(operation: () -> JSONObject, done: (Result<JSONObject>) -> Unit) {
    worker.execute { done(runCatching { failure?.let { throw it }; operation() }) }
  }

  fun awaitReadyAndReachable(cancellation: ConnectionAttempt.Cancellation) {
    val result = CompletableFuture<Result<JSONObject>>()
    execute({
      cancellation.check()
      val until = System.nanoTime() + TimeUnit.SECONDS.toNanos(2)
      var state = checked(NativeP2P.status())
      while (state.getString("state") !in listOf("Running", "NeedsLogin", "NeedsMachineAuth") && System.nanoTime() < until) {
        cancellation.check(); Thread.sleep(100); state = checked(NativeP2P.status())
      }
      check(state.getString("state") == "Running") { "Tailscale 尚未登录或未连接" }
      cancellation.check()
      checked(NativeP2P.check())
    }) { result.complete(it) }
    try { result.get(7, TimeUnit.SECONDS).getOrThrow() }
    finally { if (!result.isDone) { cancellation.cancel(); result.cancel(false) } }
  }

  fun checked(raw: String): JSONObject = JSONObject(raw).also {
    check(it.optBoolean("ok")) { it.optString("error", "连接暂时不可用") }
  }
}
