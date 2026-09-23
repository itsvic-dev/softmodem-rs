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
    description = "A V.21 modem that places real calls over SIP";
    mainProgram = "softmodem";
    license = lib.licenses.gpl3Plus;
    platforms = lib.platforms.unix;
  };
})
