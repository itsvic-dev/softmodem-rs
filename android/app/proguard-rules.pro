# SPDX-FileCopyrightText: 2026 Wiktor Bryk <contact@itsvic.dev>
#
# SPDX-License-Identifier: GPL-3.0-or-later

# app_process starts the bridge by its name.
-keep class dev.itsvic.softmodem.bridge.Bridge {
    public static void main(java.lang.String[]);
}
