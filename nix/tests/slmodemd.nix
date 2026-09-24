{
  # x86-64, where slmodemd's 32-bit x86 runs natively, under TCG on any host.
  guestPkgs,
  softmodem,
  bridge,
  slmodemd,
  label,
  # For the softmodem's +MS, and slmodemd's, which numbers the modulations.
  ours,
  theirs,
}:

let
  port = "/run/softmodem/ttyS0";
  wav = "/var/lib/softmodem/${label}.wav";
in
{
  name = "softmodem-slmodemd-${label}";

  node.pkgs = guestPkgs.lib.mkForce guestPkgs;

  nodes.machine =
    { pkgs, ... }:
    {
      environment.systemPackages = [
        pkgs.python3
        pkgs.sox
      ];

      systemd.services.softmodem = {
        wantedBy = [ "multi-user.target" ];
        serviceConfig = {
          ExecStart = "${softmodem}/bin/softmodem wire --local 127.0.0.1:5300 --pty ${port} --dump /var/lib/softmodem --init 'ATE0S0=1${ours}'";
          RuntimeDirectory = "softmodem";
          StateDirectory = "softmodem";
        };
      };

      systemd.services.slmodemd = {
        wantedBy = [ "multi-user.target" ];
        after = [ "softmodem.service" ];
        environment.SOFTMODEM_PEER = "127.0.0.1:5300";
        serviceConfig.ExecStart = "${slmodemd}/bin/slmodemd -e ${bridge}/bin/slmodem-bridge";
      };
    };

  testScript = ''
    machine.wait_for_unit("softmodem.service")
    machine.wait_for_unit("slmodemd.service")
    machine.wait_until_succeeds("test -L ${port}")
    machine.wait_until_succeeds("test -L /dev/ttySL0")

    try:
        with subtest("slmodemd calls the softmodem"):
            print(machine.succeed("python3 ${./slmodemd.py} /dev/ttySL0 ${port} '${theirs}'", timeout=600))
    finally:
        print(machine.execute("journalctl -u slmodemd -u softmodem --no-pager | tail -n 80")[1])
        machine.systemctl("stop slmodemd.service softmodem.service")
        machine.execute("cd /var/lib/softmodem && ls *-answer-rx.wav && sox -M *-answer-rx.wav *-answer-tx.wav ${wav}")
        machine.copy_from_machine("/var/lib/softmodem", "dumps")
  '';
}
