// SPDX-FileCopyrightText: 2026 Wiktor Bryk <contact@itsvic.dev>
//
// SPDX-License-Identifier: GPL-3.0-or-later

package dev.itsvic.softmodem.bridge

import java.io.File

private const val VENDOR = "/vendor/etc/audio_param/Speech_AudioParam.xml"
private const val CUSTOM = "/data/vendor/audiohal/audio_param/Speech_AudioParam.xml"
private const val MARKER = "<!-- softmodem: downlink noise reduction off -->"
private val MODE_PARAMETERS = Regex("""(<Param name="speech_mode_para" value=")([^"]*)(")""")
// speech_mode_para, as Speech_ParamUnitDesc.xml has it: "RX NR Switch" and "RX expander switch".
private const val NR_INDEX = 4
private const val NR_BITS = 0x4
private const val EXPANDER_INDEX = 5
private const val EXPANDER_BITS = 0x3

/**
 * Turns off the downlink noise reduction and expander of MediaTek's speech
 * processing while a modem call is up. They take FSK for noise.
 */
class SpeechTuning(private val setParameters: (String) -> Unit, private val exec: (Array<String>) -> Unit) {
    /** True if the phone has the file to tune, and it is now in force. */
    fun apply(): Boolean {
        val vendor = File(VENDOR)
        if (!vendor.canRead()) return false
        val tuned = MODE_PARAMETERS.replace(vendor.readText()) { match ->
            val (start, values, end) = match.destructured
            start + tune(values) + end
        }
        val custom = File(CUSTOM)
        custom.writeText(tuned.replaceFirst("<AudioParam", "$MARKER\n<AudioParam"))
        exec(arrayOf("chown", "audioserver:audio", CUSTOM))
        exec(arrayOf("chmod", "644", CUSTOM))
        exec(arrayOf("restorecon", CUSTOM))
        setParameters("SET_CUST_XML_ENABLE=1")
        return true
    }

    /** Puts the phone's own processing back, if this class changed it. */
    fun restore() {
        val custom = File(CUSTOM)
        if (!custom.exists() || !custom.readText().contains(MARKER)) return
        custom.delete()
        setParameters("SET_CUST_XML_ENABLE=0")
    }

    private fun tune(values: String): String {
        val words = values.split(',').toMutableList()
        if (words.size <= EXPANDER_INDEX) return values
        words[NR_INDEX] = clear(words[NR_INDEX], NR_BITS)
        words[EXPANDER_INDEX] = clear(words[EXPANDER_INDEX], EXPANDER_BITS)
        return words.joinToString(",")
    }

    private fun clear(word: String, bits: Int): String {
        val value = word.trim().removePrefix("0x").toIntOrNull(16) ?: return word
        return "0x%X".format(value and bits.inv())
    }
}
