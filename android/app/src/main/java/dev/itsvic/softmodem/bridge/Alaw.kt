// SPDX-FileCopyrightText: 2026 Wiktor Bryk <contact@itsvic.dev>
//
// SPDX-License-Identifier: GPL-3.0-or-later

package dev.itsvic.softmodem.bridge

/** G.711 A-law, as softmodem-transport codes it. */
object Alaw {
    private val SEGMENT_ENDS = intArrayOf(0x1F, 0x3F, 0x7F, 0xFF, 0x1FF, 0x3FF, 0x7FF, 0xFFF)

    fun encode(sample: Int): Byte {
        var value = sample shr 3
        val mask = if (value >= 0) {
            0xD5
        } else {
            value = -value - 1
            0x55
        }
        val segment = SEGMENT_ENDS.indexOfFirst { value <= it }
        if (segment < 0) return (0x7F xor mask).toByte()
        val shift = if (segment < 2) 1 else segment
        val step = (value shr shift) and 0x0F
        return ((segment shl 4 or step) xor mask).toByte()
    }

    fun decode(code: Byte): Short {
        val bits = (code.toInt() and 0xFF) xor 0x55
        val segment = (bits and 0x70) shr 4
        val step = (bits and 0x0F) shl 4
        val magnitude = if (segment == 0) step + 8 else (step + 0x108) shl (segment - 1)
        return (if (bits and 0x80 == 0) -magnitude else magnitude).toShort()
    }
}
