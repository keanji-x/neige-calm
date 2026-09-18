package io.neigecalm.next

internal object NativeP2P {
  init { System.loadLibrary("neige_p2p") }
  external fun configure(snapshot: String): String
  external fun start(directory: String): String
  external fun status(): String
  external fun direct(origin: String): String
  external fun stopDirect()
  external fun check(origin: String): String
  external fun enroll(payload: String): String
  external fun cancelEnrollment(): String
  external fun resetEnrollment(): String
  external fun tailnet(origin: String): String
}
