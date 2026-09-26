# SPDX-FileCopyrightText: 2026 Wiktor Bryk <contact@itsvic.dev>
#
# SPDX-License-Identifier: GPL-3.0-or-later

{
  guestPkgs,
  # Whether the guest runs slmodemd's 32-bit x86 under qemu-user.
  emulated,
  softmodem,
  bridge,
  slmodemd,
  label,
  # For the softmodem's +MS, and slmodemd's, which numbers the modulations.
  ours,
  theirs,
  # Noise from `after` seconds past the answer, of RMS `rms`, then a second exchange.
  noise ? null,
  # ATO1 from the softmodem after the first exchange, then a second.
  retrain ? false,
  # Wire options for the softmodem's end, such as --gateway-after on what it sends to slmodemd.
  impairment ? "",
  # The softmodem dials, and slmodemd answers with ATA.
  answer ? false,
}:

let
  port = "/run/softmodem/ttyS0";
  wav = "/var/lib/softmodem/${label}.wav";
  role = if answer then "originate" else "answer";
  dialling = if answer then "--peer 127.0.0.1:5301" else "";
  autoanswer = if answer then "" else "S0=1";
  later =
    if retrain then
      " ato1"
    else if noise == null then
      ""
    else
      " ${toString noise.later}";
in
{
  name = "softmodem-slmodemd-${label}";

  node.pkgs = guestPkgs.lib.mkForce guestPkgs;

  nodes.machine =
    { pkgs, ... }:
    {
      # Emulated, one CPU cannot run slmodemd's V.90 and the softmodem in real time.
      virtualisation.cores = 4;
      # Not i686-linux, whose magic is EM_486: slmodemd's ELF says EM_386.
      boot.binfmt.emulatedSystems = guestPkgs.lib.optional emulated "i386-linux";
      environment.systemPackages = [
        pkgs.python3
        pkgs.sox
      ];

      systemd.services.softmodem = {
        wantedBy = [ "multi-user.target" ];
        serviceConfig = {
          ExecStart = "${softmodem}/bin/softmodem wire --local 127.0.0.1:5300 --pty ${port} --dump /var/lib/softmodem ${dialling} ${impairment} --init 'ATE0${autoanswer}${ours}'";
          RuntimeDirectory = "softmodem";
          StateDirectory = "softmodem";
        };
      };

      systemd.services.slmodemd = {
        wantedBy = [ "multi-user.target" ];
        after = [ "softmodem.service" ] ++ guestPkgs.lib.optional emulated "systemd-binfmt.service";
        environment = {
          SOFTMODEM_PEER = "127.0.0.1:5300";
          SOFTMODEM_LISTEN = "127.0.0.1:5301";
        }
        // guestPkgs.lib.optionalAttrs (noise != null) {
          SOFTMODEM_NOISE_AFTER = toString noise.after;
          SOFTMODEM_NOISE_RMS = toString noise.rms;
          SOFTMODEM_NOISE_WAY = noise.way or "both";
        };
        serviceConfig = {
          ExecStart = "${slmodemd}/bin/slmodemd -d9 -e ${bridge}/bin/slmodem-bridge";
        }
        // guestPkgs.lib.optionalAttrs emulated {
          # slmodemd locks all its future memory, and under qemu-user that includes the translated code.
          LimitMEMLOCK = "infinity";
        };
      };
    };

  testScript = ''
    machine.wait_for_unit("softmodem.service")
    machine.wait_for_unit("slmodemd.service")
    machine.wait_until_succeeds("test -L ${port}")
    machine.wait_until_succeeds("test -L /dev/ttySL0")

    import os

    # The call's result goes in $out/status, so that a failed call still keeps its recording.
    status, output = machine.execute("python3 ${./slmodemd.py} /dev/ttySL0 ${port} '${theirs}'${later}${guestPkgs.lib.optionalString answer " --answer"} 2>&1", timeout=600)
    print(output)
    machine.systemctl("stop slmodemd.service softmodem.service")
    machine.succeed("journalctl -u slmodemd -u softmodem --no-pager > /var/lib/softmodem/journal.txt")
    machine.execute("cd /var/lib/softmodem && sox -M *-${role}-rx.wav *-${role}-tx.wav ${wav}")
    machine.copy_from_machine("/var/lib/softmodem", "dumps")
    with open(os.path.join(os.environ["out"], "status"), "w") as file:
        file.write("ok\n" if status == 0 else output)
  '';
}
