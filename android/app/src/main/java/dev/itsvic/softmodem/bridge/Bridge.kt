// SPDX-FileCopyrightText: 2026 Wiktor Bryk <contact@itsvic.dev>
//
// SPDX-License-Identifier: GPL-3.0-or-later

package dev.itsvic.softmodem.bridge

import android.content.Context
import android.media.AudioAttributes
import android.media.AudioFormat
import android.media.AudioManager
import android.media.AudioRecord
import android.media.AudioTrack
import android.net.Uri
import android.os.Looper
import android.os.Process
import java.net.DatagramPacket
import java.net.DatagramSocket
import java.net.InetSocketAddress
import java.net.SocketAddress
import java.net.SocketTimeoutException
import java.util.concurrent.atomic.AtomicBoolean
import kotlin.random.Random

private const val RATE = 8000
private const val FRAME = 160
private const val VOICE_DOWNLINK = 3
private const val FOREGROUND_ACTIVE = "mForegroundCallState=1"
// Telecom waits this long for each comma before it keys the digits after it.
private const val TELECOM_PAUSE_MS = 3000L
private const val MODEM_SILENT_MS = 5000L

// MediaTek's audio HAL: BGS mixes app playback into a call, muted towards the far end by default.
private val CALL_PARAMETERS = listOf("Set_BGS_UL_Mute=0", "Set_BGS_DL_Mute=1", "Set_SpeechCall_UL_Mute=1")
private val NORMAL_PARAMETERS = listOf("Set_BGS_UL_Mute=1", "Set_BGS_DL_Mute=0", "Set_SpeechCall_UL_Mute=0")

/** Carries a cellular voice call to a softmodem on the UDP wire, as root: `Bridge <port> <uplink gain>`. */
object Bridge {
    private lateinit var audio: AudioManager
    private val tuning = SpeechTuning(::setParameters) { command -> exec(*command) }

    @JvmStatic
    fun main(args: Array<String>) {
        Looper.prepareMainLooper()
        val thread = Class.forName("android.app.ActivityThread").getMethod("systemMain").invoke(null)
        val context = thread.javaClass.getMethod("getSystemContext").invoke(thread) as Context
        audio = context.getSystemService(Context.AUDIO_SERVICE) as AudioManager
        tuning.restore()
        serve(args[0].toInt(), args[1].toFloat())
    }

    private fun serve(port: Int, uplinkGain: Float) {
        val socket = DatagramSocket(InetSocketAddress("127.0.0.1", port))
        println("bridge: listening on 127.0.0.1:$port")
        while (true) {
            val (peer, number) = nextDial(socket)
            println("bridge: dialling $number")
            if (!placeCall(socket, peer, number)) continue
            send(socket, peer, "ANSWER")
            println("bridge: in call")
            try {
                call(socket, peer, uplinkGain)
            } finally {
                NORMAL_PARAMETERS.forEach(::setParameters)
                tuning.restore()
            }
            println("bridge: call ended")
        }
    }

    private fun nextDial(socket: DatagramSocket): Pair<SocketAddress, String> {
        val buffer = ByteArray(2048)
        socket.soTimeout = 0
        while (true) {
            val packet = DatagramPacket(buffer, buffer.size)
            socket.receive(packet)
            val text = String(buffer, 0, packet.length, Charsets.ISO_8859_1)
            if (text.startsWith("DIAL ")) return packet.socketAddress to text.removePrefix("DIAL ")
        }
    }

    // True once the call is up and its digits keyed, false if it failed or the modem gave up.
    private fun placeCall(socket: DatagramSocket, peer: SocketAddress, number: String): Boolean {
        if (!tuning.apply()) println("bridge: no speech parameters to tune")
        exec("am", "start", "-a", "android.intent.action.CALL", "-d", "tel:" + Uri.encode(number))
        var abandoned = false
        val gaveUp = {
            abandoned = abandoned || hungUp(socket, peer)
            abandoned
        }
        val inCall = { audio.mode == AudioManager.MODE_IN_CALL }
        val answered = waitFor(15_000) { gaveUp() || inCall() } &&
            waitFor(60_000) { gaveUp() || !inCall() || active() } &&
            inCall() && !abandoned
        if (answered) {
            CALL_PARAMETERS.forEach(::setParameters)
            audio.isSpeakerphoneOn = false
            val keyed = System.currentTimeMillis() + digitsTime(number)
            waitFor(digitsTime(number) + 1000) { gaveUp() || System.currentTimeMillis() >= keyed }
        }
        if (answered && !abandoned) return true
        println(if (abandoned) "bridge: the modem hung up" else "bridge: not answered")
        NORMAL_PARAMETERS.forEach(::setParameters)
        tuning.restore()
        endCall()
        if (!abandoned) send(socket, peer, "BUSY")
        return false
    }

    private fun digitsTime(number: String): Long {
        val after = number.substringAfter(',', "")
        if (after.isEmpty()) return 0
        val pauses = after.count { it == ',' } + 1
        return TELECOM_PAUSE_MS * pauses + 100L * (after.length - pauses + 1) + 500
    }

    private enum class Heard { NOTHING, AUDIO, BYE }

    private fun call(socket: DatagramSocket, peer: SocketAddress, uplinkGain: Float) {
        val minIn = AudioRecord.getMinBufferSize(RATE, AudioFormat.CHANNEL_IN_MONO, AudioFormat.ENCODING_PCM_16BIT)
        val minOut = AudioTrack.getMinBufferSize(RATE, AudioFormat.CHANNEL_OUT_MONO, AudioFormat.ENCODING_PCM_16BIT)
        // A second of buffer each way: the HAL takes and gives audio in bursts.
        val recorder = downlinkRecorder(maxOf(minIn, RATE * 2))
        @Suppress("DEPRECATION")
        val track = AudioTrack(
            AudioManager.STREAM_MUSIC, RATE, AudioFormat.CHANNEL_OUT_MONO,
            AudioFormat.ENCODING_PCM_16BIT, maxOf(minOut, RATE * 2), AudioTrack.MODE_STREAM,
        )
        val running = AtomicBoolean(true)
        val downlink = Thread { sendDownlink(recorder, socket, peer, running) }
        downlink.start()
        val modemHungUp = playUplink(socket, peer, track, uplinkGain, running)
        running.set(false)
        downlink.join()
        println("bridge: ${track.underrunCount} underruns")
        track.stop()
        track.release()
        if (!modemHungUp) repeat(3) { send(socket, peer, "BYE") }
        endCall()
    }

    // Plays what the modem sends into the call, until either end hangs up. True if the modem did.
    private fun playUplink(
        socket: DatagramSocket,
        peer: SocketAddress,
        track: AudioTrack,
        gain: Float,
        running: AtomicBoolean,
    ): Boolean {
        Process.setThreadPriority(Process.THREAD_PRIORITY_URGENT_AUDIO)
        val buffer = ByteArray(2048)
        val samples = ShortArray(1024)
        val packet = DatagramPacket(buffer, buffer.size)
        val prefill = RATE * 3 / 10
        var written = 0
        var lastHeard = System.currentTimeMillis()
        var lastModeCheck = 0L
        socket.soTimeout = 250
        while (running.get()) {
            val now = System.currentTimeMillis()
            if (now - lastModeCheck > 500) {
                lastModeCheck = now
                if (audio.mode != AudioManager.MODE_IN_CALL) return false
            }
            if (now - lastHeard > MODEM_SILENT_MS) return false
            when (receive(socket, peer, packet)) {
                Heard.BYE -> return true
                Heard.NOTHING -> continue
                Heard.AUDIO -> lastHeard = now
            }
            val count = packet.length - 12
            for (i in 0 until count) samples[i] = clamp(Alaw.decode(buffer[12 + i]) * gain)
            track.write(samples, 0, count)
            written += count
            if (track.playState != AudioTrack.PLAYSTATE_PLAYING && written >= prefill) track.play()
        }
        return false
    }

    private fun receive(socket: DatagramSocket, peer: SocketAddress, packet: DatagramPacket): Heard {
        packet.length = packet.data.size
        try {
            socket.receive(packet)
        } catch (_: SocketTimeoutException) {
            return Heard.NOTHING
        }
        val data = packet.data
        return when {
            packet.socketAddress != peer -> Heard.NOTHING
            packet.length == 3 && String(data, 0, 3, Charsets.ISO_8859_1) == "BYE" -> Heard.BYE
            packet.length <= 12 || data[0].toInt() and 0xC0 != 0x80 || data[1].toInt() and 0x7F != 8 -> Heard.NOTHING
            else -> Heard.AUDIO
        }
    }

    private fun sendDownlink(
        recorder: AudioRecord,
        socket: DatagramSocket,
        peer: SocketAddress,
        running: AtomicBoolean,
    ) {
        Process.setThreadPriority(Process.THREAD_PRIORITY_URGENT_AUDIO)
        val samples = ShortArray(FRAME)
        val rtp = ByteArray(12 + FRAME)
        var sequence = Random.nextInt(0x10000)
        var timestamp = Random.nextInt()
        val ssrc = Random.nextInt()
        rtp[0] = 0x80.toByte()
        rtp[1] = 8
        for (i in 0 until 4) rtp[8 + i] = (ssrc shr (24 - 8 * i)).toByte()
        val packet = DatagramPacket(rtp, rtp.size, peer)
        val agc = Agc()
        recorder.startRecording()
        while (running.get()) {
            var read = 0
            while (read < FRAME && running.get()) {
                val n = recorder.read(samples, read, FRAME - read)
                if (n < 0) {
                    running.set(false)
                    break
                }
                read += n
            }
            rtp[2] = (sequence shr 8).toByte()
            rtp[3] = sequence.toByte()
            for (i in 0 until 4) rtp[4 + i] = (timestamp shr (24 - 8 * i)).toByte()
            agc.apply(samples)
            for (i in 0 until FRAME) rtp[12 + i] = Alaw.encode(samples[i].toInt())
            try {
                socket.send(packet)
            } catch (_: Exception) {
                running.set(false)
            }
            sequence = (sequence + 1) and 0xFFFF
            timestamp += FRAME
        }
        recorder.stop()
        recorder.release()
    }

    // The public constructors refuse VOICE_DOWNLINK, so the source goes in behind their back.
    private fun downlinkRecorder(bufferSize: Int): AudioRecord {
        val attributes = AudioAttributes.Builder().build()
        AudioAttributes::class.java.getDeclaredField("mSource").apply {
            isAccessible = true
            setInt(attributes, VOICE_DOWNLINK)
        }
        val format = AudioFormat.Builder()
            .setSampleRate(RATE)
            .setChannelMask(AudioFormat.CHANNEL_IN_MONO)
            .setEncoding(AudioFormat.ENCODING_PCM_16BIT)
            .build()
        return AudioRecord::class.java
            .getConstructor(AudioAttributes::class.java, AudioFormat::class.java, Int::class.java, Int::class.java)
            .newInstance(attributes, format, bufferSize, 0)
    }

    private fun setParameters(keyValues: String) {
        Class.forName("android.media.AudioSystem")
            .getMethod("setParameters", String::class.java)
            .invoke(null, keyValues)
    }

    private fun hungUp(socket: DatagramSocket, peer: SocketAddress): Boolean {
        val buffer = ByteArray(64)
        val packet = DatagramPacket(buffer, buffer.size)
        socket.soTimeout = 1
        return try {
            socket.receive(packet)
            packet.socketAddress == peer && String(buffer, 0, packet.length, Charsets.ISO_8859_1) == "BYE"
        } catch (_: SocketTimeoutException) {
            false
        }
    }

    private fun active(): Boolean {
        val process = Runtime.getRuntime().exec(arrayOf("dumpsys", "telephony.registry"))
        val active = process.inputStream.bufferedReader().useLines { lines ->
            lines.any { it.contains(FOREGROUND_ACTIVE) }
        }
        process.waitFor()
        return active
    }

    private fun waitFor(millis: Long, condition: () -> Boolean): Boolean {
        val end = System.currentTimeMillis() + millis
        while (System.currentTimeMillis() < end) {
            if (condition()) return true
            Thread.sleep(250)
        }
        return false
    }

    private fun endCall() {
        if (audio.mode == AudioManager.MODE_IN_CALL) exec("input", "keyevent", "KEYCODE_ENDCALL")
    }

    private fun exec(vararg command: String) {
        Runtime.getRuntime().exec(command).waitFor()
    }

    private fun send(socket: DatagramSocket, peer: SocketAddress, text: String) {
        val data = text.toByteArray(Charsets.ISO_8859_1)
        socket.send(DatagramPacket(data, data.size, peer))
    }

    private fun clamp(value: Float): Short = value.toInt().coerceIn(-32768, 32767).toShort()
}
