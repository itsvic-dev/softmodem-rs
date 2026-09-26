// SPDX-FileCopyrightText: 2026 Wiktor Bryk <contact@itsvic.dev>
//
// SPDX-License-Identifier: GPL-3.0-or-later

package dev.itsvic.softmodem

import android.content.Context
import android.content.pm.ApplicationInfo
import android.util.Log
import java.io.File
import java.io.IOException
import kotlin.concurrent.thread

private const val TAG = "softmodem"
private const val BRIDGE_PORT = 5300
private const val MODEM_PORT = 5301
private const val UPLINK_GAIN = "0.5"

/** The softmodem binary and the root bridge that places its calls as the phone's cellular calls. */
class ModemProcesses(private val context: Context) {
    @Volatile private var modem: Process? = null
    private var bridge: Process? = null

    val alive: Boolean get() = modem?.isAlive == true && bridge?.isAlive == true

    /**
     * Starts both, with the modem's serial port given by [port], such as `--tcp 127.0.0.1:5302`,
     * and its stored profile by [init]. The modem runs as root when [root] is set.
     */
    @Synchronized
    fun start(port: List<String>, init: String = "", root: Boolean = false) {
        kill()
        val app = context.applicationInfo
        bridge = log(
            "bridge",
            ProcessBuilder(
                "su", "-c",
                "CLASSPATH=${app.sourceDir} exec app_process /system/bin dev.itsvic.softmodem.bridge.Bridge " +
                    "$BRIDGE_PORT $UPLINK_GAIN",
            ),
        )
        val arguments = mutableListOf(
            File(app.nativeLibraryDir, "libsoftmodem.so").path, "wire",
            "--local", "127.0.0.1:$MODEM_PORT",
            "--peer", "127.0.0.1:$BRIDGE_PORT",
        )
        arguments += port
        if (init.isNotEmpty()) arguments += listOf("--init", init)
        if (app.flags and ApplicationInfo.FLAG_DEBUGGABLE != 0) {
            arguments += listOf("--dump", File(context.filesDir, "dumps").path)
        }
        val builder = if (root) {
            ProcessBuilder("su", "-c", "NO_COLOR=1 exec " + arguments.joinToString(" ", transform = ::quote))
        } else {
            ProcessBuilder(arguments).apply { environment()["NO_COLOR"] = "1" }
        }
        modem = log("modem", builder)
    }

    /** Waits until the modem exits. */
    fun waitFor() {
        modem?.waitFor()
    }

    /** Stops both, if this started them. */
    @Synchronized
    fun stop() {
        if (modem != null || bridge != null) kill()
    }

    private fun kill() {
        modem?.destroy()
        bridge?.destroy()
        modem = null
        bridge = null
        // Both can run under su, beyond the reach of destroy().
        runCatching {
            ProcessBuilder("su", "-c", "pkill -f dev.itsvic.softmodem.bridge.Bridge; pkill -f libsoftmodem.so")
                .start().waitFor()
        }
    }

    private fun log(name: String, builder: ProcessBuilder): Process {
        val process = builder.redirectErrorStream(true).start()
        thread(isDaemon = true, name = name) {
            try {
                process.inputStream.bufferedReader().forEachLine { Log.i(TAG, "$name: $it") }
            } catch (_: IOException) {
                // destroy() closes the stream under the reader.
            }
        }
        return process
    }
}

private fun quote(argument: String) = "'" + argument.replace("'", "'\\''") + "'"
