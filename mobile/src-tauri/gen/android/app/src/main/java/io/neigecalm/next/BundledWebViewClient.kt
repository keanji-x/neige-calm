package io.neigecalm.next

import android.graphics.Bitmap
import android.net.http.SslError
import android.os.Message
import android.view.KeyEvent
import android.webkit.*
import java.io.ByteArrayInputStream

/** Keep the exact Wry client alive: Ipc reads its currentUrl for native authority. */
@Suppress("DEPRECATION")
internal class BundledWebViewClient(
  private val original: WebViewClient,
  private val assets: BundledFrontendAssets,
) : WebViewClient() {
  override fun shouldInterceptRequest(view: WebView, request: WebResourceRequest): WebResourceResponse? {
    // The original also tracks interception and injects initialization scripts
    // on WebViews without document-start support. Preserve that bookkeeping.
    val response = original.shouldInterceptRequest(view, request)
    if (!HttpOriginFence.permits(request.url.toString(), assets.origin.get())) return WebResourceResponse(
      "text/plain", "UTF-8", 403, "Unconfigured HTTP origin", mapOf("Cache-Control" to "no-store"), ByteArrayInputStream(ByteArray(0)))
    return response ?: assets.response(request)
  }

  override fun shouldInterceptRequest(view: WebView, url: String): WebResourceResponse? {
    if (!HttpOriginFence.permits(url, assets.origin.get())) return WebResourceResponse("text/plain", "UTF-8", 403, "Unconfigured HTTP origin", mapOf("Cache-Control" to "no-store"), ByteArrayInputStream(ByteArray(0)))
    return original.shouldInterceptRequest(view, url)
  }
  override fun shouldOverrideUrlLoading(view: WebView, request: WebResourceRequest): Boolean = !HttpOriginFence.permits(request.url.toString(), assets.origin.get()) || original.shouldOverrideUrlLoading(view, request)
  override fun shouldOverrideUrlLoading(view: WebView, url: String): Boolean = !HttpOriginFence.permits(url, assets.origin.get()) || original.shouldOverrideUrlLoading(view, url)
  override fun onPageStarted(view: WebView, url: String, favicon: Bitmap?) = original.onPageStarted(view, url, favicon)
  override fun onPageFinished(view: WebView, url: String) {
    original.onPageFinished(view, url)
    val origin = assets.origin.get()
    val uri = runCatching { java.net.URI(url) }.getOrNull()
    if (origin != null && uri != null && origin.matches(uri)) RememberedSession.persist(origin.value)
  }
  override fun onPageCommitVisible(view: WebView, url: String) = original.onPageCommitVisible(view, url)
  override fun onLoadResource(view: WebView, url: String) = original.onLoadResource(view, url)
  override fun doUpdateVisitedHistory(view: WebView, url: String, isReload: Boolean) = original.doUpdateVisitedHistory(view, url, isReload)
  override fun onReceivedError(view: WebView, request: WebResourceRequest, error: WebResourceError) = original.onReceivedError(view, request, error)
  override fun onReceivedError(view: WebView, code: Int, description: String, url: String) = original.onReceivedError(view, code, description, url)
  override fun onReceivedHttpError(view: WebView, request: WebResourceRequest, response: WebResourceResponse) = original.onReceivedHttpError(view, request, response)
  override fun onReceivedSslError(view: WebView, handler: SslErrorHandler, error: SslError) = original.onReceivedSslError(view, handler, error)
  override fun onReceivedClientCertRequest(view: WebView, request: ClientCertRequest) = original.onReceivedClientCertRequest(view, request)
  override fun onReceivedHttpAuthRequest(view: WebView, handler: HttpAuthHandler, host: String, realm: String) = original.onReceivedHttpAuthRequest(view, handler, host, realm)
  override fun onFormResubmission(view: WebView, dontResend: Message, resend: Message) = original.onFormResubmission(view, dontResend, resend)
  override fun onTooManyRedirects(view: WebView, cancel: Message, proceed: Message) = original.onTooManyRedirects(view, cancel, proceed)
  override fun shouldOverrideKeyEvent(view: WebView, event: KeyEvent): Boolean = original.shouldOverrideKeyEvent(view, event)
  override fun onUnhandledKeyEvent(view: WebView, event: KeyEvent) = original.onUnhandledKeyEvent(view, event)
  override fun onScaleChanged(view: WebView, oldScale: Float, newScale: Float) = original.onScaleChanged(view, oldScale, newScale)
  override fun onReceivedLoginRequest(view: WebView, realm: String, account: String?, args: String) = original.onReceivedLoginRequest(view, realm, account, args)
  override fun onRenderProcessGone(view: WebView, detail: RenderProcessGoneDetail): Boolean = original.onRenderProcessGone(view, detail)
  override fun onSafeBrowsingHit(view: WebView, request: WebResourceRequest, threatType: Int, callback: SafeBrowsingResponse) = original.onSafeBrowsingHit(view, request, threatType, callback)
}
