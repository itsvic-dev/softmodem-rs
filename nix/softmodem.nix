# SPDX-FileCopyrightText: 2026 Wiktor Bryk <contact@itsvic.dev>
#
# SPDX-License-Identifier: GPL-3.0-or-later

{
  lib,
  pkgs,
}:

let
  cargoNix = import ./Cargo.nix { inherit pkgs; };
in
cargoNix.workspaceMembers.softmodem.build.overrideAttrs (old: {
  name = "softmodem-${old.version}";
  pname = "softmodem";

  meta = {
    description = "A software modem that places real calls over SIP";
    mainProgram = "softmodem";
    license = lib.licenses.gpl3Plus;
    platforms = lib.platforms.unix;
  };
})
