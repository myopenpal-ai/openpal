package com.myopenpal.openpal

import android.app.Activity
import android.content.Context
import android.net.wifi.WifiManager
import app.tauri.annotation.Command
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSObject
import app.tauri.plugin.Plugin

/**
 * Holds a `WifiManager.MulticastLock` while the Remote Access client
 * (`crate::remote::client::discovery`) is browsing for a paired desktop's
 * mDNS advertisement. Many OEM Wi-Fi stacks otherwise drop multicast
 * packets before they ever reach the app, per `WifiManager`'s own docs on
 * `createMulticastLock`. Foreground-only: acquired when browsing starts,
 * released when it stops — see `crates/app/src/remote_client_android.rs`
 * for the Rust side that calls this, mirroring `SecretStorePlugin.kt`'s
 * shape for the same kind of Android-only OS capability.
 */
@TauriPlugin
class WifiLockPlugin(private val activity: Activity) : Plugin(activity) {
  private var lock: WifiManager.MulticastLock? = null

  private fun wifiManager() =
    activity.applicationContext.getSystemService(Context.WIFI_SERVICE) as WifiManager

  @Command
  fun acquire(invoke: Invoke) {
    try {
      if (lock == null) {
        val newLock = wifiManager().createMulticastLock("openpal-remote-discovery")
        newLock.setReferenceCounted(false)
        newLock.acquire()
        lock = newLock
      }
      invoke.resolve(JSObject())
    } catch (e: Exception) {
      invoke.reject("couldn't acquire the Wi-Fi multicast lock: ${e.message}")
    }
  }

  @Command
  fun release(invoke: Invoke) {
    try {
      lock?.let { if (it.isHeld) it.release() }
      lock = null
      invoke.resolve(JSObject())
    } catch (e: Exception) {
      invoke.reject("couldn't release the Wi-Fi multicast lock: ${e.message}")
    }
  }
}
