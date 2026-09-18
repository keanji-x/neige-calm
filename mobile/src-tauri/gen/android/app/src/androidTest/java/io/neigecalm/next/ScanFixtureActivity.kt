package io.neigecalm.next

import android.app.Activity
import android.content.Context
import android.content.ContextWrapper
import android.os.Build
import android.os.Bundle
import android.os.Process
import android.os.SystemClock
import android.view.View
import android.view.ViewGroup
import android.view.WindowManager
import android.view.inspector.WindowInspector
import android.webkit.WebView
import android.widget.Button
import android.widget.TextView
import androidx.lifecycle.Lifecycle
import androidx.test.core.app.ActivityScenario
import androidx.test.platform.app.InstrumentationRegistry
import androidx.test.espresso.Espresso.onView
import androidx.test.espresso.action.ViewActions.click
import androidx.test.espresso.matcher.RootMatchers.withDecorView
import androidx.test.espresso.matcher.ViewMatchers.withId
import androidx.test.espresso.matcher.ViewMatchers.withText
import org.hamcrest.Matchers.allOf
import org.hamcrest.Matchers.sameInstance
import org.json.JSONArray
import org.json.JSONObject
import org.junit.Assert.*
import java.net.URI
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicReference

/** One explicitly owned Activity per runner for scan boundary fixtures.
 * Real recreation/process-death acceptance remains in the recovery tests.
 * Never close Tauri's last Activity inside its own instrumentation process.
 */
internal object ScanFixtureActivity {
  private var held: ActivityScenario<MainActivity>? = null
  fun acquire(label: String): ActivityScenario<MainActivity> {
    emit(JSONObject().put("stage", "$label.acquire").put("reuse", held != null).put("state", held?.state?.name))
    val scenario = held ?: ActivityScenario.launch(MainActivity::class.java).also { held = it }
    check(scenario.state != Lifecycle.State.DESTROYED) { "Shared test Activity was destroyed; inspect lifecycle diagnostics" }
    scenario.moveToState(Lifecycle.State.RESUMED)
    scenario.onActivity { emit(snapshot("$label.resumed", it)) }
    return scenario
  }
  fun pause(scenario: ActivityScenario<MainActivity>, label: String) {
    scenario.onActivity { emit(snapshot("$label.before-pause", it)) }
    scenario.moveToState(Lifecycle.State.CREATED)
    scenario.onActivity { emit(snapshot("$label.paused", it)) }
  }
  fun findWebView(view: View?): WebView? {
    if (view is WebView) return view
    if (view is ViewGroup) for (i in 0 until view.childCount) findWebView(view.getChildAt(i))?.let { return it }
    return null
  }
  private fun safeUrl(raw: String?): String? = raw?.let {
    if (it == "about:blank") return@let it
    if (it.startsWith("data:")) return@let "data:[redacted]"
    runCatching { URI(it).let { uri -> "${uri.scheme}://${uri.rawAuthority ?: ""}${uri.rawPath ?: ""}" } }.getOrDefault("invalid-url")
  }
  private fun views(root: View): List<View> = buildList {
    add(root)
    if (root is ViewGroup) for (i in 0 until root.childCount) addAll(views(root.getChildAt(i)))
  }
  private fun contextOwner(context: Context): Activity? {
    var current = context
    repeat(16) {
      if (current is Activity) return current as Activity
      val wrapped = current as? ContextWrapper ?: return null
      if (wrapped.baseContext === current) return null
      current = wrapped.baseContext
    }
    return null
  }
  private fun roots(): List<View> = if (Build.VERSION.SDK_INT >= 29) WindowInspector.getGlobalWindowViews() else emptyList()
  private fun owned(root: View, host: MainActivity): Boolean {
    val token = (root.layoutParams as? WindowManager.LayoutParams)?.token
    return contextOwner(root.context) === host || (token != null && token == host.window.decorView.windowToken)
  }
  fun snapshot(stage: String, host: MainActivity, webView: WebView? = findWebView(host.findViewById(android.R.id.content))): JSONObject {
    val windows = JSONArray()
    for (root in roots()) {
      val button = root.findViewById<Button>(android.R.id.button1)
      windows.put(JSONObject().put("type", (root.layoutParams as? WindowManager.LayoutParams)?.type)
        .put("owned", owned(root, host)).put("attached", root.isAttachedToWindow).put("shown", root.isShown)
        .put("focus", root.hasWindowFocus()).put("width", root.width).put("height", root.height)
        .put("resetTitle", views(root).filterIsInstance<TextView>().any { it.text.toString() == "重新开始手机入网？" })
        .put("resetButton", button?.text?.toString() == "退出并重新扫码")
        .put("buttonEnabled", button?.isEnabled).put("buttonShown", button?.isShown))
    }
    return JSONObject().put("stage", stage).put("activity", System.identityHashCode(host))
      .put("lifecycle", host.lifecycle.currentState.name).put("finishing", host.isFinishing).put("destroyed", host.isDestroyed)
      .put("activityFocus", host.hasWindowFocus()).put("windowVisibility", host.window.decorView.windowVisibility)
      .put("nativeUrl", safeUrl(webView?.url)).put("originalUrl", safeUrl(webView?.originalUrl))
      .put("progress", webView?.progress).put("viewAttached", webView?.isAttachedToWindow).put("windows", windows)
  }
  fun emit(value: JSONObject) {
    value.put("pid", Process.myPid()).put("elapsedMs", SystemClock.elapsedRealtime())
    InstrumentationRegistry.getInstrumentation().sendStatus(2, Bundle().also { it.putString("s4_fixture", value.toString()) })
  }
  fun evaluate(scenario: ActivityScenario<MainActivity>, view: WebView, script: String, timeoutMs: Long = 3000): String {
    val result = AtomicReference(""); val done = CountDownLatch(1)
    scenario.onActivity { view.evaluateJavascript(script) { result.set(it); done.countDown() } }
    assertTrue("WebView evaluation did not complete within ${timeoutMs}ms", done.await(timeoutMs, TimeUnit.MILLISECONDS))
    return result.get()
  }
  fun awaitUi(scenario: ActivityScenario<MainActivity>, label: String, condition: (MainActivity) -> Boolean) {
    val deadline = SystemClock.elapsedRealtime() + 10000
    var last = JSONObject()
    while (SystemClock.elapsedRealtime() < deadline) {
      var ready = false
      scenario.onActivity { host -> ready = condition(host); last = snapshot(label, host) }
      if (ready) { emit(last.put("ready", true)); return }
      SystemClock.sleep(50)
    }
    emit(last.put("ready", false))
    fail("Fixture condition not reached within 10s: $last")
  }
  fun awaitDocument(scenario: ActivityScenario<MainActivity>, view: WebView, expectedUrl: String) {
    val deadline = SystemClock.elapsedRealtime() + 10000
    var native = JSONObject(); var js = ""
    while (SystemClock.elapsedRealtime() < deadline) {
      var exact = false
      scenario.onActivity { host -> exact = view.url == expectedUrl; native = snapshot("chooser.document", host, view) }
      val remaining = deadline-SystemClock.elapsedRealtime()
      if (remaining <= 0) break
      try {
        js = evaluate(scenario, view, "({origin:location.origin,path:location.pathname,ready:document.readyState,visibility:document.visibilityState})", minOf(3000,remaining))
      } catch (error: AssertionError) {
        emit(native.put("evaluationError",error.message))
        throw AssertionError("Document evaluation failed: $native",error)
      }
      val state = runCatching { JSONObject(js) }.getOrNull()
      if (exact && state?.optString("origin") == "https://scan-file.invalid" && state.optString("path") == "/next/" && state.optString("ready") == "complete") {
        emit(native.put("js", state).put("ready", true)); return
      }
      SystemClock.sleep(50)
    }
    emit(native.put("js", js).put("ready", false))
    fail("Production bundled document must retain exact native URL and JS origin within 10s: $native")
  }
  fun confirmOwnedResetDialog(scenario: ActivityScenario<MainActivity>) {
    check(Build.VERSION.SDK_INT >= 29) { "This API35 fixture needs WindowInspector diagnostics (API29+)" }
    val deadline = SystemClock.elapsedRealtime() + 10000
    var last = JSONObject()
    while (SystemClock.elapsedRealtime() < deadline) {
      var selected: View? = null
      scenario.onActivity { host ->
        last = snapshot("reset.confirmation", host)
        val candidates = roots().filter { root -> owned(root, host) &&
          views(root).filterIsInstance<TextView>().any { it.text.toString() == "重新开始手机入网？" } }
        if (candidates.size == 1) {
          val root = candidates.single()
          val button = root.findViewById<Button>(android.R.id.button1)
          if (root.isAttachedToWindow && root.isShown && root.width > 0 && root.height > 0 &&
            button != null && button.text.toString() == "退出并重新扫码" && button.isShown && button.isEnabled && button.width > 0 && button.height > 0) {
            emit(last.put("ready", true))
            selected = root
          }
        }
      }
      selected?.let { root ->
        try {
          // Preserve Espresso's focus/touch checks and default timeout. Bind
          // to the actual dialog after creation instead of selecting an older
          // Activity root while the asynchronous native command is pending.
          onView(allOf(withId(android.R.id.button1),withText("退出并重新扫码")))
            .inRoot(withDecorView(sameInstance(root))).perform(click())
        } catch (error: Throwable) {
          scenario.onActivity { emit(snapshot("reset.espresso-click-failed",it)) }
          throw error
        }
        return
      }
      SystemClock.sleep(50)
    }
    emit(last.put("ready", false))
    fail("Reset must create an owned, visible, enabled confirmation within 10s; Espresso focus timeout is unchanged: $last")
  }
  fun cancelOwnedResetDialog(host: MainActivity) {
    for (root in roots().filter { owned(it,host) && views(it).filterIsInstance<TextView>().any { text -> text.text.toString()=="重新开始手机入网？" } }) {
      val cancel = root.findViewById<Button>(android.R.id.button2)
      if (cancel?.text?.toString()=="取消" && cancel.isEnabled) cancel.performClick()
    }
  }
}
