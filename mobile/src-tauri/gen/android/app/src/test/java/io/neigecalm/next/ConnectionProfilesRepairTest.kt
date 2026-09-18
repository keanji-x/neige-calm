package io.neigecalm.next

import android.content.SharedPreferences
import java.lang.reflect.Proxy
import org.junit.Assert.*
import org.junit.Test

/** Storage only is substituted; every read/repair runs the production owner. */
class ConnectionProfilesRepairTest {
  private class Store {
    val values = mutableMapOf<String, Any>()
    var writable = true
    val preferences = Proxy.newProxyInstance(SharedPreferences::class.java.classLoader,
      arrayOf(SharedPreferences::class.java)) { _, method, args ->
      val key = args?.firstOrNull() as? String
      when (method.name) {
        "contains" -> values.containsKey(key)
        "getString" -> (values[key] ?: args!![1]) as String?
        "getBoolean" -> (values[key] ?: args!![1]) as Boolean
        "getInt" -> (values[key] ?: args!![1]) as Int
        "getLong" -> (values[key] ?: args!![1]) as Long
        "edit" -> editor()
        else -> error("Unexpected storage call: ${method.name}")
      }
    } as SharedPreferences
    private fun editor(): SharedPreferences.Editor {
      val pending = mutableMapOf<String, Any>()
      return Proxy.newProxyInstance(SharedPreferences.Editor::class.java.classLoader,
        arrayOf(SharedPreferences.Editor::class.java)) { proxy, method, args ->
        when (method.name) {
          "putString", "putInt", "putLong", "putBoolean" -> {
            pending[args!![0] as String] = args[1]; proxy
          }
          "commit" -> { if (writable) values.putAll(pending); writable }
          else -> error("Unexpected storage write: ${method.name}")
        }
      } as SharedPreferences.Editor
    }
  }
  private val oldOrigin = "https://old.tail.example"
  private val nextOrigin = "https://next.tail.example"
  private val direct = "https://direct.example"
  private fun corruptions(): List<Pair<String, Any>> = listOf(
    "tailnet-origin" to 7, "tailnet-origin" to "http://untrusted.example",
    "tailnet-origins" to true, "tailnet-origins" to "{broken",
    "tailnet-origins" to "[17]", "tailscale-enabled" to "true",
    "ip-origin" to 7, "mode" to false, "config-revision" to "broken",
  )
  @Test fun explicitSaveRepairsMalformedFieldsAndRetiresOldResumeIdentity() {
    for ((field, value) in corruptions()) {
      val store = Store(); val profiles = ConnectionProfiles(store.preferences)
      profiles.selectTailnet(oldOrigin)
      profiles.save("ip", direct, true)
      val id = profiles.profileId(); val revision = profiles.revision()
      store.values[field] = value
      assertTrue("$field=$value must fail closed", runCatching { profiles.read(); profiles.tailnetOrigins() }.isFailure)
      profiles.save("ip", direct, false)
      assertEquals("$field=$value", direct, profiles.read().ipOrigin)
      assertFalse(profiles.read().tailscaleEnabled)
      assertTrue("Old resume must be invalid", profiles.profileId() != id || profiles.revision() != revision)
      profiles.tailnetOrigins()
    }
  }
  @Test fun verifiedScanRepairsMalformedFieldsAndPreservesValidDirectProfile() {
    for ((field, value) in corruptions()) {
      val store = Store(); val profiles = ConnectionProfiles(store.preferences)
      profiles.selectTailnet(oldOrigin); profiles.save("ip", direct, true)
      val id = profiles.profileId(); val revision = profiles.revision()
      store.values[field] = value
      profiles.selectTailnet(nextOrigin)
      assertEquals(nextOrigin, profiles.read().tailnetOrigin)
      assertTrue(profiles.read().tailscaleEnabled)
      assertEquals(if (field == "ip-origin") "" else direct, profiles.read().ipOrigin)
      assertTrue(profiles.tailnetOrigins().contains(nextOrigin))
      assertTrue("Old resume must be invalid", profiles.profileId() != id || profiles.revision() != revision)
    }
  }
  @Test fun repairDoesNotReportSuccessWhenAtomicCommitFails() {
    val store = Store(); val profiles = ConnectionProfiles(store.preferences)
    profiles.selectTailnet(oldOrigin)
    store.values["tailnet-origin"] = 7
    val before = store.values.toMap(); store.writable = false
    assertTrue(runCatching { profiles.selectTailnet(nextOrigin) }.isFailure)
    assertEquals(before, store.values)
  }
}
