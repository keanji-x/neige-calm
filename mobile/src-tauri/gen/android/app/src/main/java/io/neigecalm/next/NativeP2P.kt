package io.neigecalm.next

internal object NativeP2P {
  init { System.loadLibrary("neige_p2p") }
  external fun configure(snapshot: String): String
  external fun start(directory: String): String
  external fun status(): String
  external fun direct(origin: String, binding: String): String
  external fun checkDirect(token: String, origin: String, binding: String, confirm: Boolean): String
  external fun stopDirect()
  external fun check(origin: String): String
  external fun reserveOperation(): String
  external fun cancelOperation(token: String): String
  external fun confirmLegacy(token: String, origin: String): String
  external fun enroll(token: String, payload: String): String
  external fun cancelEnrollment(): String
  external fun resetEnrollment(token: String): String
  external fun tailnet(origin: String): String
}
