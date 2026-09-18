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
  private fun binding(origin: String, address: String = "192.168.1.8") = """{"schemaVersion":1,"origin":"$origin","addresses":["$address"]}"""
  @Test fun confirmedDirectBindingPersistsWithoutBeingReplacedByPassiveSaves() {
    val store = Store(); val profiles = ConnectionProfiles(store.preferences)
    profiles.save("ip", direct, false)
    val before = profiles.revision()
    profiles.confirmDirectBinding(direct, binding(direct))
    assertTrue(profiles.revision() > before)
    val confirmed = profiles.revision()
    profiles.save("tailscale", direct, false)
    assertEquals(binding(direct), ConnectionProfiles(store.preferences).directBinding(direct))
    profiles.selectTailnet(nextOrigin)
    assertEquals(binding(direct), profiles.directBinding(direct))
    profiles.confirmDirectBinding(direct, binding(direct, "192.168.1.9"))
    assertTrue(profiles.revision() > confirmed)
    assertEquals(binding(direct, "192.168.1.9"), profiles.directBinding(direct))
    profiles.save("ip", "https://different.example", true)
    assertEquals("", profiles.directBinding(direct))
    assertEquals("", profiles.directBinding("https://different.example"))
  }
  @Test fun staleOrFailedDirectConfirmationCannotWriteAnotherProfile() {
    val store = Store(); val profiles = ConnectionProfiles(store.preferences)
    profiles.save("ip", direct, false)
    val before = store.values.toMap()
    assertTrue(runCatching { profiles.confirmDirectBinding(nextOrigin, binding(nextOrigin)) }.isFailure)
    assertEquals(before, store.values)
    store.writable = false
    assertTrue(runCatching { profiles.confirmDirectBinding(direct, binding(direct)) }.isFailure)
    assertEquals(before, store.values)
  }
  @Test fun corruptedDirectBindingDoesNotBlockLocalConfigurationAndExplicitConfirmationRepairsIt() {
    val store = Store(); val profiles = ConnectionProfiles(store.preferences)
    profiles.save("ip", direct, false)
    store.values["direct-binding"] = 7
    assertEquals(direct, profiles.read().ipOrigin)
    assertEquals("", profiles.directBinding(direct))
    profiles.confirmDirectBinding(direct, binding(direct))
    assertEquals(binding(direct), profiles.directBinding(direct))
  }
  @Test fun savedTailnetChoicePreservesDirectConfigurationButPinsItsAttempt() {
    val profiles = ConnectionProfiles(Store().preferences)
    profiles.selectTailnet(oldOrigin); profiles.selectTailnet(nextOrigin)
    profiles.save("ip", direct, true)
    val selected = profiles.selectSavedTailnet(nextOrigin)
    assertEquals(direct, selected.ipOrigin)
    val attempted = mutableListOf<ConnectionRoute>()
    val outcome = ConnectionAttempt.firstAvailable(selected, nextOrigin) { attempted.add(it) }
    assertEquals(listOf(ConnectionRoute("tailscale", nextOrigin)), attempted)
    assertEquals(nextOrigin, outcome.route?.origin)
    assertEquals(nextOrigin, ConnectionAttempt.firstAvailable(profiles.read()) {}.route?.origin)
    profiles.save("tailscale", direct, true, clearSelection = true)
    assertEquals("ip", ConnectionAttempt.firstAvailable(profiles.read()) {}.route?.mode)
  }
  @Test fun explicitSavedIntentSurvivesRetryAndReopenUntilAnExplicitModeChoice() {
    val store = Store(); val profiles = ConnectionProfiles(store.preferences)
    profiles.selectTailnet(oldOrigin); profiles.selectTailnet(nextOrigin); profiles.save("ip", direct, true)
    profiles.selectSavedTailnet(nextOrigin)
    store.values.remove("explicit-tailnet")
    assertTrue("An older saved B record must not silently fall through to A",profiles.read().explicitTailnet)
    val revision = profiles.revision()
    profiles.save("tailscale", direct, true)
    assertEquals(revision, profiles.revision())
    val reopened = ConnectionProfiles(store.preferences)
    assertTrue(reopened.read().explicitTailnet)
    val failed = ConnectionAttempt.firstAvailable(reopened.read()) { throw java.net.SocketTimeoutException("chosen unavailable") }
    assertNull(failed.route)
    assertEquals(listOf(ConnectionFailure("tailscale", "chosen unavailable")), failed.failures)
    assertEquals(nextOrigin, ConnectionAttempt.firstAvailable(reopened.read()) {}.route?.origin)
    reopened.disableTailnet()
    reopened.save("tailscale", direct, false)
    assertTrue(reopened.read().explicitTailnet)
    assertTrue(reopened.read().candidates().isEmpty())
    reopened.save("ip", direct, true)
    assertFalse(reopened.read().explicitTailnet)
    assertEquals("ip", ConnectionAttempt.firstAvailable(reopened.read()) {}.route?.mode)
  }
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
  @Test fun v1UpgradePreservesLegacyOriginAndDirectSettingsUntilConfirmedSelection() {
    val store = Store()
    store.values.putAll(mapOf("tailscale-enabled" to true, "ip-origin" to direct, "mode" to "tailscale"))
    val profiles = ConnectionProfiles(store.preferences)
    val id = profiles.profileId()
    assertEquals(P2PConnection.ORIGIN,profiles.read().tailnetOrigin)
    assertTrue(profiles.needsLegacyConfirmation())
    assertEquals(emptyList<String>(),profiles.tailnetOrigins())
    profiles.save("tailscale",direct,true)
    assertTrue("A settings save cannot pretend native migration completed",profiles.needsLegacyConfirmation())
    profiles.selectTailnet(P2PConnection.ORIGIN)
    assertEquals(id,profiles.profileId())
    assertEquals(direct,profiles.read().ipOrigin)
    assertEquals(listOf(P2PConnection.ORIGIN),profiles.tailnetOrigins())
  }
}
