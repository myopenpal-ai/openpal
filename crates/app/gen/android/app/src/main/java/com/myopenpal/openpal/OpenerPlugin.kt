package com.myopenpal.openpal

import android.app.Activity
import android.content.Intent
import android.net.Uri
import app.tauri.annotation.Command
import app.tauri.annotation.InvokeArg
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSObject
import app.tauri.plugin.Plugin

@InvokeArg
class OpenerUrlArgs {
  lateinit var url: String
}

/**
 * Hands an http(s) URL to Android's own opener (`Intent.ACTION_VIEW`) —
 * the platform's real equivalent of `commands::os_open`'s desktop
 * `/usr/bin/open`/`explorer`/`xdg-open` subprocess spawn, none of which
 * exist on Android. See `crates/app/src/opener_android.rs` for the Rust
 * side that calls this; mirrors `SecretStorePlugin.kt`/`WifiLockPlugin.kt`'s
 * shape for the same kind of Android-only OS capability.
 */
@TauriPlugin
class OpenerPlugin(private val activity: Activity) : Plugin(activity) {
  @Command
  fun open(invoke: Invoke) {
    try {
      val args = invoke.parseArgs(OpenerUrlArgs::class.java)
      val intent = Intent(Intent.ACTION_VIEW, Uri.parse(args.url))
      intent.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
      activity.startActivity(intent)
      invoke.resolve(JSObject())
    } catch (e: Exception) {
      invoke.reject("couldn't open that link: ${e.message}")
    }
  }
}
