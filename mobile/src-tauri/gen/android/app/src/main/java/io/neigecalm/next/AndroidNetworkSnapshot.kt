package io.neigecalm.next

import java.net.NetworkInterface
import org.json.JSONArray
import org.json.JSONObject

/** Android's supported Java interface APIs replace Go's restricted netlink path. */
internal object AndroidNetworkSnapshot {
  fun read(): String {
    val rows = JSONArray()
    val interfaces = NetworkInterface.getNetworkInterfaces() ?: return rows.toString()
    while (interfaces.hasMoreElements()) {
      val item = interfaces.nextElement()
      val addresses = JSONArray()
      for (address in item.interfaceAddresses) {
        val ip = address.address.hostAddress?.substringBefore('%') ?: continue
        addresses.put("$ip/${address.networkPrefixLength}")
      }
      var flags = 0
      if (item.isUp) flags = flags or 1 or 32
      if (item.isLoopback) flags = flags or 4
      if (item.isPointToPoint) flags = flags or 8
      if (item.supportsMulticast()) flags = flags or 16
      rows.put(JSONObject().put("name", item.name).put("index", item.index)
        .put("mtu", item.mtu).put("flags", flags).put("addresses", addresses))
    }
    return rows.toString()
  }
}
