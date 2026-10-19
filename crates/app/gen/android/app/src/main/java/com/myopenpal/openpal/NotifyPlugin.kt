package com.myopenpal.openpal

import android.Manifest
import android.annotation.SuppressLint
import android.app.Activity
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Intent
import android.net.Uri
import android.os.Build
import android.provider.Settings
import android.webkit.WebView
import androidx.core.app.NotificationCompat
import androidx.core.app.NotificationManagerCompat
import app.tauri.PermissionState
import app.tauri.annotation.Command
import app.tauri.annotation.InvokeArg
import app.tauri.annotation.Permission
import app.tauri.annotation.PermissionCallback
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Channel
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSObject
import app.tauri.plugin.Plugin
import java.util.concurrent.atomic.AtomicInteger

@InvokeArg
class NotifyPostArgs {
  lateinit var title: String
  lateinit var body: String
  var conversationId: String? = null
}

@InvokeArg
class NotifyWatchTapsArgs {
  lateinit var channel: Channel
}

/**
 * OS notifications on Android: posting them, the POST_NOTIFICATIONS runtime
 * permission (Android 13 and later), the app's notification settings page,
 * and taps. See `crates/app/src/notify/android.rs` for the Rust side; mirrors
 * `SecretStorePlugin.kt`'s shape for the same kind of Android-only capability.
 *
 * Every command resolves or rejects on every path: Rust waits on each call
 * with no timeout. Commands, `load` and `onNewIntent` all run on the UI
 * thread, so the fields below need no locking.
 *
 * A tap opens [MainActivity] (single task) with [EXTRA_TAP] set: through
 * `onNewIntent` while the app runs, or as the launch intent on a cold start.
 * It is sent on the channel Rust registers with `watchTaps`; one that comes
 * before that (the tap that launched the app) waits in [pendingTap].
 */
@TauriPlugin(
  permissions = [
    // The alias is PERMISSION_ALIAS below.
    Permission(strings = [Manifest.permission.POST_NOTIFICATIONS], alias = "postNotifications")
  ]
)
class NotifyPlugin(private val activity: Activity) : Plugin(activity) {
  private companion object {
    const val PERMISSION_ALIAS = "postNotifications"
    const val CHANNEL_ID = "openpal-alerts"
    const val EXTRA_TAP = "com.myopenpal.openpal.NOTIFICATION_TAP"
    const val EXTRA_CONVERSATION = "com.myopenpal.openpal.NOTIFICATION_CONVERSATION"
    // Notification ids, which are also the PendingIntent request codes: one
    // each, so two notifications for two conversations never share an intent.
    // Seeded from the clock so a new launch does not reuse, and so replace, the
    // id of one still in the tray from the last.
    val nextId = AtomicInteger((System.currentTimeMillis() and 0x3fffffff).toInt())
  }

  private var taps: Channel? = null
  private var pendingTap: JSObject? = null

  override fun load(webView: WebView) {
    super.load(webView)
    createChannel()
    takeTap(activity.intent)
  }

  override fun onNewIntent(intent: Intent) {
    super.onNewIntent(intent)
    takeTap(intent)
  }

  @Command
  fun watchTaps(invoke: Invoke) {
    val args = invoke.parseArgs(NotifyWatchTapsArgs::class.java)
    taps = args.channel
    pendingTap?.let { args.channel.send(it) }
    pendingTap = null
    invoke.resolve(JSObject())
  }

  @Command
  fun permissionState(invoke: Invoke) {
    invoke.resolve(stateObject())
  }

  @Command
  fun askPermission(invoke: Invoke) {
    // Below Android 13 the permission is granted at install, and Tauri's
    // request would never answer for one already granted; after a second "no"
    // Android will not show the prompt again. None of these has anything to ask.
    val asked = getPermissionState(PERMISSION_ALIAS)
    if (Build.VERSION.SDK_INT < Build.VERSION_CODES.TIRAMISU ||
      asked == PermissionState.GRANTED ||
      asked == PermissionState.DENIED
    ) {
      invoke.resolve(stateObject())
      return
    }
    requestPermissionForAlias(PERMISSION_ALIAS, invoke, "permissionAnswered")
  }

  @PermissionCallback
  private fun permissionAnswered(invoke: Invoke) {
    invoke.resolve(stateObject())
  }

  @Command
  fun openSettings(invoke: Invoke) {
    try {
      val intent = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
        Intent(Settings.ACTION_APP_NOTIFICATION_SETTINGS)
          .putExtra(Settings.EXTRA_APP_PACKAGE, activity.packageName)
      } else {
        Intent(
          Settings.ACTION_APPLICATION_DETAILS_SETTINGS,
          Uri.fromParts("package", activity.packageName, null)
        )
      }
      activity.startActivity(intent)
      invoke.resolve(JSObject())
    } catch (e: Exception) {
      invoke.reject("couldn't open the notification settings: ${e.message}")
    }
  }

  // `currentState()` is the permission check lint cannot see.
  @SuppressLint("MissingPermission")
  @Command
  fun post(invoke: Invoke) {
    try {
      val args = invoke.parseArgs(NotifyPostArgs::class.java)
      if (currentState() != "granted") {
        invoke.reject("NOT_ALLOWED")
        return
      }
      createChannel()
      val id = nextId.incrementAndGet()
      val open = Intent(activity, activity.javaClass).apply {
        action = Intent.ACTION_MAIN
        addCategory(Intent.CATEGORY_LAUNCHER)
        addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP or Intent.FLAG_ACTIVITY_CLEAR_TOP)
        putExtra(EXTRA_TAP, true)
        args.conversationId?.let { putExtra(EXTRA_CONVERSATION, it) }
      }
      val onTap = PendingIntent.getActivity(
        activity,
        id,
        open,
        PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT
      )
      val notification = NotificationCompat.Builder(activity, CHANNEL_ID)
        .setSmallIcon(R.drawable.ic_stat_openpal)
        .setContentTitle(args.title)
        .setContentText(args.body)
        .setContentIntent(onTap)
        .setAutoCancel(true)
        .build()
      NotificationManagerCompat.from(activity).notify(id, notification)
      invoke.resolve(JSObject())
    } catch (e: SecurityException) {
      invoke.reject("NOT_ALLOWED")
    } catch (e: Exception) {
      invoke.reject("couldn't post the notification: ${e.message}")
    }
  }

  /** What the OS allows now: "granted", "denied" (only Settings can undo it) or "prompt". */
  private fun currentState(): String {
    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
      when (getPermissionState(PERMISSION_ALIAS)) {
        PermissionState.GRANTED -> Unit
        PermissionState.DENIED -> return "denied"
        // Never asked, or refused once and still allowed to ask again.
        else -> return "prompt"
      }
    }
    // Granted (or nothing to grant) can still be switched off in Settings, for
    // the whole app or for this one channel.
    if (!NotificationManagerCompat.from(activity).areNotificationsEnabled()) return "denied"
    if (channelSwitchedOff()) return "denied"
    return "granted"
  }

  private fun stateObject() = JSObject().apply { put("state", currentState()) }

  private fun notificationManager() =
    activity.getSystemService(NotificationManager::class.java)

  /** Android 8 and later need a channel; creating it again only refreshes its name. */
  private fun createChannel() {
    if (Build.VERSION.SDK_INT < Build.VERSION_CODES.O) return
    val channel = NotificationChannel(
      CHANNEL_ID,
      activity.getString(R.string.notification_channel_name),
      NotificationManager.IMPORTANCE_DEFAULT
    )
    channel.description = activity.getString(R.string.notification_channel_description)
    notificationManager().createNotificationChannel(channel)
  }

  private fun channelSwitchedOff(): Boolean {
    if (Build.VERSION.SDK_INT < Build.VERSION_CODES.O) return false
    val channel = notificationManager().getNotificationChannel(CHANNEL_ID) ?: return false
    return channel.importance == NotificationManager.IMPORTANCE_NONE
  }

  private fun takeTap(intent: Intent?) {
    if (intent == null || !intent.getBooleanExtra(EXTRA_TAP, false)) return
    // Reopening the app from Recents hands back the intent it was launched
    // with, tap extras and all; that is not a new tap.
    if ((intent.flags and Intent.FLAG_ACTIVITY_LAUNCHED_FROM_HISTORY) != 0) return
    val tap = JSObject()
    intent.getStringExtra(EXTRA_CONVERSATION)?.let { tap.put("conversationId", it) }
    // Consumed, so the same intent is never read as a tap twice.
    intent.removeExtra(EXTRA_TAP)
    intent.removeExtra(EXTRA_CONVERSATION)
    val channel = taps
    if (channel == null) pendingTap = tap else channel.send(tap)
  }
}
