package io.neigecalm.next

import org.junit.Assert.*
import org.junit.Test

class NativeOperationTest {
  @Test fun queuedCancellationRevokesBeforeAnyPreparationOrJNI() {
    val order = mutableListOf<String>()
    val operation = NativeOperation("reserved-token") { order.add("revoke:$it") }
    operation.cancel()
    assertTrue(runCatching { operation.run({ order.add("prepare") }) { order.add("JNI:$it") } }.isFailure)
    assertEquals(listOf("revoke:reserved-token"),order)
  }
  @Test fun cancellationDuringPreparationDoesNotMintANewReservation() {
    val order = mutableListOf<String>()
    val operation = NativeOperation("reserved-token") { order.add("revoke:$it") }
    assertTrue(runCatching { operation.run({ operation.cancel() }) { order.add("JNI:$it") } }.isFailure)
    assertEquals(listOf("revoke:reserved-token"),order)
  }
  @Test fun nativeEntryAndRevocationUseTheSamePreDispatchToken() {
    val order = mutableListOf<String>()
    val operation = NativeOperation("reserved-token") { order.add("revoke:$it") }
    operation.run({ order.add("prepare") }) { token -> order.add("JNI:$token"); operation.cancel() }
    assertEquals(listOf("prepare","JNI:reserved-token","revoke:reserved-token"),order)
  }
}
