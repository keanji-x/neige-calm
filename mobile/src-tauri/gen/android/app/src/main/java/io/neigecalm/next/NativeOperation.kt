package io.neigecalm.next

/** Java owns scheduling; Go owns the one-shot admission and commit fence.
 * Reservation happens before dispatch, so cancel before JNI is not a new scan.
 */
internal class NativeOperation(private val token: String, private val revoke: (String) -> Unit) {
  @Volatile private var cancelled = false
  fun <T> run(prepare: () -> Unit, invoke: (String) -> T): T {
    checkActive()
    prepare()
    checkActive()
    return invoke(token)
  }
  private fun checkActive() {
    if (cancelled || Thread.currentThread().isInterrupted) throw java.util.concurrent.CancellationException("连接已取消")
  }
  fun cancel() { cancelled = true; revoke(token) }
  companion object {
    fun reserve(): NativeOperation {
      val token = P2PConnection.checked(NativeP2P.reserveOperation()).getString("token")
      return NativeOperation(token) { P2PConnection.checked(NativeP2P.cancelOperation(it)) }
    }
  }
}
