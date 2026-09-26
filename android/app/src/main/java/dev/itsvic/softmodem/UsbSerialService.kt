// SPDX-FileCopyrightText: 2026 Wiktor Bryk <contact@itsvic.dev>
//
// SPDX-License-Identifier: GPL-3.0-or-later

package dev.itsvic.softmodem

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.os.IBinder
import android.os.PowerManager
import kotlin.concurrent.thread
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow

// The gadget's ACM function, which MediaTek's USB configurations with adb and MTP carry.
private const val DEVICE = "/dev/ttyGS0"
// Rides out the codec's fades, which outlast the default 1.4 s, and starts on V.21, which holds up best.
private const val INIT = "ATS10=50+MS=V21,0"
private const val CHANNEL = "usb-serial"
private const val NOTIFICATION = 1
private const val ACTION_STOP = "dev.itsvic.softmodem.STOP"

/** Serves the modem to a computer on the USB cable, as the phone's USB serial port. */
class UsbSerialService : Service() {
    private val processes by lazy { ModemProcesses(this) }
    private var wakeLock: PowerManager.WakeLock? = null
    private var watcher: Thread? = null

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        if (intent?.action == ACTION_STOP) {
            stopSelf()
            return START_NOT_STICKY
        }
        if (watcher != null) return START_NOT_STICKY
        startForeground(NOTIFICATION, notification())
        wakeLock = getSystemService(PowerManager::class.java)
            .newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "softmodem:usb-serial")
            .apply { acquire() }
        _running.value = true
        watcher = thread(name = "usb-serial") {
            processes.start(listOf("--serial", DEVICE), INIT, root = true)
            processes.waitFor()
            if (watcher === Thread.currentThread()) stopSelf()
        }
        return START_NOT_STICKY
    }

    override fun onDestroy() {
        watcher = null
        thread { processes.stop() }
        wakeLock?.release()
        _running.value = false
        super.onDestroy()
    }

    private fun notification(): Notification {
        getSystemService(NotificationManager::class.java).createNotificationChannel(
            NotificationChannel(CHANNEL, "USB serial port", NotificationManager.IMPORTANCE_LOW),
        )
        val open = PendingIntent.getActivity(
            this, 0, Intent(this, MainActivity::class.java), PendingIntent.FLAG_IMMUTABLE,
        )
        val stop = PendingIntent.getService(
            this, 0, Intent(this, UsbSerialService::class.java).setAction(ACTION_STOP), PendingIntent.FLAG_IMMUTABLE,
        )
        return Notification.Builder(this, CHANNEL)
            .setSmallIcon(android.R.drawable.stat_sys_phone_call)
            .setContentTitle("Modem on the USB serial port")
            .setContentText("A computer on the cable dials with AT commands.")
            .setContentIntent(open)
            .addAction(Notification.Action.Builder(null, "Stop", stop).build())
            .setOngoing(true)
            .build()
    }

    companion object {
        private val _running = MutableStateFlow(false)
        val running: StateFlow<Boolean> = _running.asStateFlow()

        fun start(context: Context) {
            context.startForegroundService(Intent(context, UsbSerialService::class.java))
        }

        fun stop(context: Context) {
            context.stopService(Intent(context, UsbSerialService::class.java))
        }
    }
}
