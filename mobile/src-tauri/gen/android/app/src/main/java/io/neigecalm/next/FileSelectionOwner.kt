package io.neigecalm.next

/** One callback per document. Late Android results never adopt a newer request. */
internal class FileSelectionOwner<T>(private val isCurrent: () -> Boolean) {
  private class Request<T>(val deliver: (T?) -> Unit) { var cancelLaunch: (() -> Unit)? = null }
  private var pending: Request<T>? = null
  private var closed = false
  fun begin(deliver: (T?) -> Unit, launch: ((T?) -> Unit) -> (() -> Unit)) {
    cancel()
    if (closed || !isCurrent()) { deliver(null); return }
    val request = Request<T>(deliver)
    pending = request
    try {
      val release = launch { value ->
        if (pending === request) {
          pending = null
          request.cancelLaunch?.invoke()
          request.deliver(if (!closed && isCurrent()) value else null)
        }
      }
      if (pending === request) request.cancelLaunch = release else release()
    } catch (_: Exception) { if (pending === request) cancel() }
  }
  fun cancel() {
    val request = pending ?: return
    pending = null
    try { request.cancelLaunch?.invoke() } finally { request.deliver(null) }
  }
  fun dispose() { closed = true; cancel() }
}
