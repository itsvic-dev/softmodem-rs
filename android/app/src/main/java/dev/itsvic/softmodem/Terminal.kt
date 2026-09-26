// SPDX-FileCopyrightText: 2026 Wiktor Bryk <contact@itsvic.dev>
//
// SPDX-License-Identifier: GPL-3.0-or-later

package dev.itsvic.softmodem

/** What the far end has sent, as lines of text. ANSI escape sequences are dropped. */
class Terminal(private val maxLines: Int = 500) {
    private val lines = ArrayDeque<String>()
    private val current = StringBuilder()
    private var returned = false
    private var completed = 0
    private var escape = Escape.NONE

    private enum class Escape { NONE, STARTED, CSI }

    /** Takes bytes from the modem, and gives the lines they complete. */
    fun feed(bytes: ByteArray, count: Int = bytes.size): List<String> {
        val before = completed
        for (i in 0 until count) feed(bytes[i].toInt() and 0xFF)
        return lines.toList().takeLast(minOf(completed - before, lines.size))
    }

    private fun feed(byte: Int) {
        if (escape != Escape.NONE) {
            escape = when {
                escape == Escape.STARTED && byte == '['.code -> Escape.CSI
                escape == Escape.CSI && byte !in 0x40..0x7E -> Escape.CSI
                else -> Escape.NONE
            }
            return
        }
        when (byte) {
            0x1B -> escape = Escape.STARTED
            '\n'.code -> newLine()
            '\r'.code -> returned = true
            0x08, 0x7F -> if (current.isNotEmpty()) current.setLength(current.length - 1)
            else -> if (byte >= 0x20) {
                if (returned) current.setLength(0)
                returned = false
                current.append(byte.toChar())
            }
        }
    }

    private fun newLine() {
        lines.addLast(current.toString())
        completed++
        current.setLength(0)
        returned = false
        while (lines.size > maxLines) lines.removeFirst()
    }

    fun text(): String = (lines + current.toString()).joinToString("\n")
}
