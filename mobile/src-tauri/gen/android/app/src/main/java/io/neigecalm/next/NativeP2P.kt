package io.neigecalm.next

internal object NativeP2P {
  init { System.loadLibrary("neige_p2p") }
  external fun configure(snapshot: String): String
  external fun start(directory: String): String
  external fun status(): String
  external fun direct(origin: String): String
  external fun stopDirect()
  external fun check(): String
  external fun probe(): String
  external fun login(): String
  external fun proxy(): String
}
