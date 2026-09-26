// SPDX-FileCopyrightText: 2026 Wiktor Bryk <contact@itsvic.dev>
//
// SPDX-License-Identifier: GPL-3.0-or-later

package dev.itsvic.softmodem

import android.app.Application
import android.content.pm.ApplicationInfo
import android.util.Log
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import java.io.File
import java.io.IOException
import java.io.InputStream
import java.net.InetSocketAddress
import java.net.Socket
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

private const val TAG = "softmodem"
private const val BRIDGE_PORT = 5300
private const val MODEM_PORT = 5301
private const val SERIAL_PORT = 5302
private const val UPLINK_GAIN = "0.5"
private val FAILURES = listOf("NO CARRIER", "BUSY", "NO ANSWER", "NO DIALTONE", "ERROR")

enum class Modulation(val label: String, val command: String) {
    V21("V.21, 300 bit/s", "V21,0"),
    V22("V.22, 1200 bit/s", "V22,0"),
    V22BIS("V.22bis, 2400 bit/s", "V22B,0"),
}

sealed interface CallState {
    data object Idle : CallState
    data object Dialling : CallState
    data class Connected(val result: String) : CallState
    data class Ended(val result: String) : CallState
}

/** The modem and the bridge to the phone's calls, and the one call they carry. */
class ModemViewModel(application: Application) : AndroidViewModel(application) {
    private val terminal = Terminal()
    private val _text = MutableStateFlow("")
    private val _state = MutableStateFlow<CallState>(CallState.Idle)
    val text: StateFlow<String> = _text.asStateFlow()
    val state: StateFlow<CallState> = _state.asStateFlow()

    val number = MutableStateFlow("")
    val modulation = MutableStateFlow(Modulation.V21)

    private var modem: Process? = null
    private var bridge: Process? = null
    private var serial: Socket? = null

    fun dial() {
        val number = number.value.filter { it.isDigit() || it in ",*#ABCD" }
        if (number.isEmpty() || _state.value is CallState.Dialling || _state.value is CallState.Connected) return
        _state.value = CallState.Dialling
        viewModelScope.launch(Dispatchers.IO) {
            try {
                val socket = serial ?: connect()
                // Rides out the codec's fades, which outlast the default 1.4 s.
                write(socket, "ATE0V1S10=50\r")
                write(socket, "AT+MS=${modulation.value.command}\r")
                write(socket, "ATDT$number\r")
            } catch (error: IOException) {
                Log.w(TAG, "dial failed", error)
                show("\n${error.message}\n")
                _state.value = CallState.Ended("ERROR")
            }
        }
    }

    fun send(line: String) {
        val socket = serial ?: return
        viewModelScope.launch(Dispatchers.IO) {
            runCatching { write(socket, "$line\r") }
        }
    }

    /** Hangs up as DTR dropping does: the modem ends the call when its port closes. */
    fun hangUp() {
        val socket = serial ?: return
        serial = null
        runCatching { socket.close() }
        if (_state.value !is CallState.Ended) _state.value = CallState.Ended("NO CARRIER")
    }

    fun inCall(): Boolean = _state.value is CallState.Dialling || _state.value is CallState.Connected

    fun reset() {
        if (_state.value is CallState.Ended) _state.value = CallState.Idle
    }

    private suspend fun connect(): Socket {
        start()
        repeat(50) {
            try {
                val socket = Socket()
                socket.connect(InetSocketAddress("127.0.0.1", SERIAL_PORT))
                serial = socket
                viewModelScope.launch(Dispatchers.IO) { read(socket) }
                return socket
            } catch (_: IOException) {
                delay(100)
            }
        }
        throw IOException("the modem did not start")
    }

    private suspend fun start() = withContext(Dispatchers.IO) {
        if (modem?.isAlive == true && bridge?.isAlive == true) return@withContext
        stop()
        val app = getApplication<Application>().applicationInfo
        val binary = File(app.nativeLibraryDir, "libsoftmodem.so").path
        bridge = log(
            "bridge",
            ProcessBuilder(
                "su", "-c",
                "CLASSPATH=${app.sourceDir} exec app_process /system/bin dev.itsvic.softmodem.bridge.Bridge " +
                    "$BRIDGE_PORT $UPLINK_GAIN",
            ),
        )
        val arguments = mutableListOf(
            binary, "wire",
            "--local", "127.0.0.1:$MODEM_PORT",
            "--peer", "127.0.0.1:$BRIDGE_PORT",
            "--tcp", "127.0.0.1:$SERIAL_PORT",
        )
        if (app.flags and ApplicationInfo.FLAG_DEBUGGABLE != 0) {
            arguments += listOf("--dump", File(getApplication<Application>().filesDir, "dumps").path)
        }
        modem = log("modem", ProcessBuilder(arguments).apply { environment()["NO_COLOR"] = "1" })
    }

    private fun log(name: String, builder: ProcessBuilder): Process {
        val process = builder.redirectErrorStream(true).start()
        viewModelScope.launch(Dispatchers.IO) {
            process.inputStream.bufferedReader().forEachLine { Log.i(TAG, "$name: $it") }
        }
        return process
    }

    private fun stop() {
        serial?.let { runCatching { it.close() } }
        serial = null
        modem?.destroy()
        bridge?.destroy()
        // The bridge runs under su, beyond the reach of destroy().
        runCatching {
            ProcessBuilder("su", "-c", "pkill -f dev.itsvic.softmodem.bridge.Bridge; pkill -f libsoftmodem.so")
                .start().waitFor()
        }
    }

    private fun read(socket: Socket) {
        val input: InputStream = socket.getInputStream()
        val buffer = ByteArray(1024)
        while (true) {
            val n = try {
                input.read(buffer)
            } catch (_: IOException) {
                -1
            }
            if (n < 0) break
            synchronized(terminal) {
                terminal.feed(buffer, n).forEach { result(it.trim()) }
                _text.value = terminal.text()
            }
        }
        if (serial === socket) serial = null
    }

    private fun result(line: String) {
        when {
            line.startsWith("CONNECT") -> _state.value = CallState.Connected(line)
            FAILURES.any { line.startsWith(it) } && _state.value !is CallState.Idle ->
                _state.value = CallState.Ended(line)
        }
    }

    private fun show(text: String) = synchronized(terminal) {
        terminal.feed(text.toByteArray())
        _text.value = terminal.text()
    }

    private fun write(socket: Socket, text: String) {
        socket.getOutputStream().apply {
            write(text.toByteArray(Charsets.ISO_8859_1))
            flush()
        }
    }

    override fun onCleared() {
        stop()
    }
}
