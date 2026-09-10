package io.neigecalm.next

import android.content.Context
import org.json.JSONObject
import java.io.File
import java.util.concurrent.Executors

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

  fun checked(raw: String): JSONObject = JSONObject(raw).also {
    check(it.optBoolean("ok")) { it.optString("error", "连接暂时不可用") }
  }
}
