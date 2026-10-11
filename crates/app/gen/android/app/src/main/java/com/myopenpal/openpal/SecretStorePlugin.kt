package com.myopenpal.openpal

import android.app.Activity
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import androidx.security.crypto.EncryptedSharedPreferences
import androidx.security.crypto.MasterKey
import app.tauri.annotation.Command
import app.tauri.annotation.InvokeArg
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSObject
import app.tauri.plugin.Plugin

@InvokeArg
class SecretStoreKeyArgs {
  lateinit var key: String
}

@InvokeArg
class SecretStoreStoreArgs {
  lateinit var key: String
  lateinit var value: String
}

/**
 * Android Keystore-backed replacement for the `keyring` crate, which has no
 * real Android backend (see docs/design/MOBILE.md and
 * crates/core/src/crypto/secret_store.rs). One shared, Keystore-encrypted
 * SharedPreferences file, entries addressed by the same `key` strings
 * KeyringSecretStore uses on desktop/iOS ("master-key",
 * "account-refresh-token") — see crates/app/src/secret_store_android.rs for
 * the Rust side that calls this.
 */
@TauriPlugin
class SecretStorePlugin(private val activity: Activity) : Plugin(activity) {
  private fun prefs() = run {
    val masterKey = MasterKey.Builder(activity)
      .setKeyGenParameterSpec(
        KeyGenParameterSpec.Builder(
          MasterKey.DEFAULT_MASTER_KEY_ALIAS,
          KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT
        )
          .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
          .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
          .setKeySize(256)
          .build()
      )
      .build()
    EncryptedSharedPreferences.create(
      activity,
      "openpal_secret_store",
      masterKey,
      EncryptedSharedPreferences.PrefKeyEncryptionScheme.AES256_SIV,
      EncryptedSharedPreferences.PrefValueEncryptionScheme.AES256_GCM
    )
  }

  @Command
  fun store(invoke: Invoke) {
    try {
      val args = invoke.parseArgs(SecretStoreStoreArgs::class.java)
      prefs().edit().putString(args.key, args.value).apply()
      invoke.resolve(JSObject())
    } catch (e: Exception) {
      invoke.reject("couldn't save to the Android Keystore: ${e.message}")
    }
  }

  @Command
  fun load(invoke: Invoke) {
    try {
      val args = invoke.parseArgs(SecretStoreKeyArgs::class.java)
      val value = prefs().getString(args.key, null)
      if (value == null) {
        // Distinct, stable string the Rust side matches on to distinguish
        // "no entry" (KeyLookup::Absent) from every other failure
        // (KeyLookup::Unavailable) — see secret_store_android.rs.
        invoke.reject("NOT_FOUND")
      } else {
        val ret = JSObject()
        ret.put("value", value)
        invoke.resolve(ret)
      }
    } catch (e: Exception) {
      invoke.reject("couldn't reach the Android Keystore: ${e.message}")
    }
  }

  @Command
  fun delete(invoke: Invoke) {
    try {
      val args = invoke.parseArgs(SecretStoreKeyArgs::class.java)
      prefs().edit().remove(args.key).apply()
      invoke.resolve(JSObject())
    } catch (e: Exception) {
      invoke.reject("couldn't remove the entry: ${e.message}")
    }
  }
}
