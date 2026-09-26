// SPDX-FileCopyrightText: 2026 Wiktor Bryk <contact@itsvic.dev>
//
// SPDX-License-Identifier: GPL-3.0-or-later

package dev.itsvic.softmodem

import android.content.Intent
import android.os.Bundle
import android.view.KeyEvent
import androidx.activity.ComponentActivity
import androidx.activity.compose.BackHandler
import androidx.activity.compose.setContent
import androidx.activity.viewModels
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.RadioButton
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.darkColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.input.key.Key
import androidx.compose.ui.input.key.KeyEventType
import androidx.compose.ui.input.key.key
import androidx.compose.ui.input.key.onPreviewKeyEvent
import androidx.compose.ui.input.key.type
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalView
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

class MainActivity : ComponentActivity() {
    private val modem: ModemViewModel by viewModels()

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContent {
            MaterialTheme(colorScheme = darkColorScheme()) {
                Surface(Modifier.fillMaxSize()) { App(modem) }
            }
        }
    }

    // The system's in-call screen covers the terminal whenever a call starts.
    private val bringBack = Runnable {
        startActivity(Intent(this, MainActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_REORDER_TO_FRONT))
    }

    override fun onPause() {
        super.onPause()
        if (modem.inCall()) window.decorView.postDelayed(bringBack, 1000)
    }

    override fun onResume() {
        super.onResume()
        window.decorView.removeCallbacks(bringBack)
    }

    override fun onKeyDown(keyCode: Int, event: KeyEvent): Boolean {
        if (keyCode == KeyEvent.KEYCODE_CALL) {
            modem.dial()
            return true
        }
        return super.onKeyDown(keyCode, event)
    }
}

@Composable
private fun App(modem: ModemViewModel) {
    val state by modem.state.collectAsState()
    val usbSerial by UsbSerialService.running.collectAsState()
    val serving = usbSerial
    when {
        serving != null -> UsbSerialScreen(serving)
        state is CallState.Idle -> DialScreen(modem)
        else -> TerminalScreen(modem, state)
    }
}

@Composable
private fun UsbSerialScreen(modulation: Modulation) {
    val context = LocalContext.current
    Column(Modifier.fillMaxSize().padding(8.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
        Text("The modem is on the USB serial port.", style = MaterialTheme.typography.titleMedium)
        Text("Up to ${modulation.label}.", style = MaterialTheme.typography.bodyMedium)
        Text(
            "A computer on the cable sees it as /dev/ttyACM0 on Linux, and dials with AT commands, " +
                "such as ATDT0300. It stays on when you leave the app.",
            style = MaterialTheme.typography.bodySmall,
        )
        Button(onClick = { UsbSerialService.stop(context) }, Modifier.fillMaxWidth()) { Text("Stop") }
    }
}

@Composable
private fun DialScreen(modem: ModemViewModel) {
    val number by modem.number.collectAsState()
    val modulation by modem.modulation.collectAsState()
    val focus = remember { FocusRequester() }
    LaunchedEffect(Unit) { focus.requestFocus() }

    Column(
        Modifier.fillMaxSize().padding(8.dp).verticalScroll(rememberScrollState()),
        verticalArrangement = Arrangement.spacedBy(6.dp),
    ) {
        OutlinedTextField(
            value = number,
            onValueChange = { modem.number.value = it },
            label = { Text("Number") },
            singleLine = true,
            keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Phone, imeAction = ImeAction.Go),
            keyboardActions = KeyboardActions(onGo = { modem.dial() }),
            modifier = Modifier.fillMaxWidth().focusRequester(focus),
        )
        Text(
            "Each comma waits 3 s. Digits after one are keyed once the call is answered, " +
                "for a menu: 0300,,,1234#",
            style = MaterialTheme.typography.bodySmall,
        )
        Modulation.entries.forEach { option ->
            Row(
                Modifier.fillMaxWidth().selectable(
                    selected = option == modulation,
                    onClick = { modem.modulation.value = option },
                    role = Role.RadioButton,
                ),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                RadioButton(selected = option == modulation, onClick = null)
                Text(option.label, Modifier.padding(start = 4.dp))
            }
        }
        if (modulation != Modulation.V21) {
            Text(
                "Mobile voice codecs damage ${modulation.label.substringBefore(',')}, so expect " +
                    "garbled text and lost calls. V.21 holds up best.",
                color = MaterialTheme.colorScheme.error,
                style = MaterialTheme.typography.bodySmall,
            )
        }
        Button(onClick = { modem.dial() }, Modifier.fillMaxWidth()) { Text("Dial") }
        val context = LocalContext.current
        OutlinedButton(
            onClick = {
                modem.stop()
                UsbSerialService.start(context, modulation)
            },
            modifier = Modifier.fillMaxWidth(),
        ) { Text("Serve a computer on USB") }
    }
}

@Composable
private fun TerminalScreen(modem: ModemViewModel, state: CallState) {
    val text by modem.text.collectAsState()
    val scroll = rememberScrollState()
    var line by rememberSaveable { mutableStateOf("") }
    val ended = state is CallState.Ended
    val view = LocalView.current
    DisposableEffect(ended) {
        view.keepScreenOn = !ended
        onDispose { view.keepScreenOn = false }
    }
    LaunchedEffect(text) { scroll.scrollTo(scroll.maxValue) }
    BackHandler { if (ended) modem.reset() else modem.hangUp() }

    Column(Modifier.fillMaxSize().padding(4.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
        Text(
            when (state) {
                CallState.Dialling -> "Dialling…"
                is CallState.Connected -> state.result
                is CallState.Ended -> "${state.result}. Back for a new call."
                CallState.Idle -> ""
            },
            style = MaterialTheme.typography.labelMedium,
        )
        Box(Modifier.weight(1f).fillMaxWidth().verticalScroll(scroll)) {
            Text(text, fontFamily = FontFamily.Monospace, fontSize = 12.sp, lineHeight = 14.sp)
        }
        if (!ended) {
            val connected = state is CallState.Connected
            val send = {
                modem.send(line)
                line = ""
            }
            OutlinedTextField(
                value = line,
                onValueChange = { line = it },
                singleLine = true,
                enabled = connected,
                keyboardOptions = KeyboardOptions(imeAction = ImeAction.Send),
                keyboardActions = KeyboardActions(onSend = { send() }),
                modifier = Modifier.fillMaxWidth().onPreviewKeyEvent { event ->
                    val sends = event.key == Key.Enter || event.key == Key.NumPadEnter || event.key == Key.DirectionCenter
                    if (sends && event.type == KeyEventType.KeyUp) send()
                    sends
                },
            )
            Row(horizontalArrangement = Arrangement.spacedBy(4.dp)) {
                Button(onClick = { send() }, enabled = connected, modifier = Modifier.weight(1f)) { Text("Send") }
                Button(onClick = { modem.hangUp() }, Modifier.weight(1f)) { Text("Hang up") }
            }
        } else {
            Button(onClick = { modem.reset() }, Modifier.fillMaxWidth()) { Text("New call") }
        }
    }
}
