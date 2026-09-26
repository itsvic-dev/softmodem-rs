// SPDX-FileCopyrightText: 2026 Wiktor Bryk <contact@itsvic.dev>
//
// SPDX-License-Identifier: GPL-3.0-or-later

package dev.itsvic.softmodem.bridge

import kotlin.math.sqrt

/** Holds a signal at [target] RMS, slow enough to leave a modem's own modulation alone. */
class Agc(
    private val target: Float = 2000f,
    private val minGain: Float = 0.25f,
    private val maxGain: Float = 8f,
    private val frames: Int = 50,
) {
    private var meanSquare = target * target
    var gain = 1f
        private set

    fun apply(samples: ShortArray, count: Int = samples.size) {
        var sum = 0.0
        for (i in 0 until count) sum += samples[i] * samples[i].toDouble()
        meanSquare += ((sum / count).toFloat() - meanSquare) / frames
        gain = (target / sqrt(meanSquare.coerceAtLeast(1f))).coerceIn(minGain, maxGain)
        for (i in 0 until count) {
            samples[i] = (samples[i] * gain).toInt().coerceIn(-32768, 32767).toShort()
        }
    }
}
