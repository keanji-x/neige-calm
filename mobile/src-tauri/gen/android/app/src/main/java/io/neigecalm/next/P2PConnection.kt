package io.neigecalm.next

import android.content.Context
import org.json.JSONObject
import java.io.File
import java.util.concurrent.Executors
import java.util.concurrent.CompletableFuture
import java.util.concurrent.TimeUnit

/** One node and bounded recovery flight per process; never deletes node identity. */
internal object P2PConnection {
  const val ORIGIN = "https://pivot-neige.tail328551.ts.net:10000"
  private val worker = Executors.newSingleThreadScheduledExecutor()
  private var task: java.util.concurrent.ScheduledFuture<*>? = null
  private var epoch = 0
  private var retry = 0
  private var foreground = true
  private var inFlight = false
  private var pendingWake = false

  /** Native start binds loopback first and starts tsnet asynchronously. Safe off the UI thread. */
  fun prepare(context: Context) {
    checked(NativeP2P.configure(AndroidNetworkSnapshot.read()))
    checked(NativeP2P.start(File(context.applicationContext.noBackupFilesDir, "p2p-node").absolutePath))
  }
  fun start(context: Context) = wake(context)
  @Synchronized fun wake(context: Context) {
    foreground = true
    if (inFlight) { pendingWake = true; return }
    task?.cancel(false); task = null
    val generation = ++epoch
    val app = context.applicationContext
    schedule(app, generation, 0)
  }
  @Synchronized fun pause() { foreground = false; epoch++; task?.cancel(false); task = null; pendingWake = false }
  @Synchronized private fun schedule(context: Context, generation: Int, delay: Long) {
    if (!foreground || generation != epoch) return
    task = worker.schedule(work@{
      synchronized(this) {
        if (!foreground || generation != epoch) return@work
        inFlight = true; task = null
      }
      val result = runCatching { prepare(context); checked(NativeP2P.status()).getString("state") }
      synchronized(this) {
        inFlight = false
        if (!foreground) return@synchronized
        if (generation != epoch || pendingWake) {
          pendingWake = false; schedule(context, epoch, 0); return@synchronized
        }
        if (result.getOrNull() == "Running") { retry = 0; return@synchronized }
        if (result.getOrNull() in listOf("NeedsLogin", "NeedsMachineAuth")) return@synchronized
        val wait = (minOf(30000L, 500L shl minOf(retry++, 6)) * (0.75 + Math.random() * 0.5)).toLong()
        schedule(context, generation, wait)
      }
    }, delay, TimeUnit.MILLISECONDS)
  }
  fun execute(operation: () -> JSONObject, done: (Result<JSONObject>) -> Unit) {
    worker.execute { done(runCatching { checked(NativeP2P.configure(AndroidNetworkSnapshot.read())); operation() }) }
  }
  fun awaitReadyAndReachable(origin: String, cancellation: ConnectionAttempt.Cancellation) {
    val result = CompletableFuture<Result<JSONObject>>()
    execute({
      cancellation.check()
      val until = System.nanoTime() + TimeUnit.SECONDS.toNanos(2)
      var state = checked(NativeP2P.status())
      while (state.getString("state") !in listOf("Running", "NeedsLogin", "NeedsMachineAuth") && System.nanoTime() < until) {
        cancellation.check(); Thread.sleep(100); state = checked(NativeP2P.status())
      }
      check(state.getString("state") == "Running") { "Tailscale 尚未登录或未连接" }
      cancellation.check(); checked(NativeP2P.check(origin))
    }) { result.complete(it) }
    try { result.get(12, TimeUnit.SECONDS).getOrThrow() }
    finally { if (!result.isDone) { cancellation.cancel(); result.cancel(false) } }
  }
  fun checked(raw: String): JSONObject = JSONObject(raw).also {
    check(it.optBoolean("ok")) { it.optString("error", "连接暂时不可用") }
  }
}
