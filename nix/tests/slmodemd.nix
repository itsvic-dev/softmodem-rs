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
        serviceConfig.ExecStart = "${slmodemd}/bin/slmodemd -d9 -e ${bridge}/bin/slmodem-bridge";
      };
    };

  testScript = ''
    machine.wait_for_unit("softmodem.service")
    machine.wait_for_unit("slmodemd.service")
    machine.wait_until_succeeds("test -L ${port}")
    machine.wait_until_succeeds("test -L /dev/ttySL0")

    import os

    # The call's result goes in $out/status, so that a failed call still keeps its recording.
    status, output = machine.execute("python3 ${./slmodemd.py} /dev/ttySL0 ${port} '${theirs}' 2>&1", timeout=600)
    print(output)
    machine.systemctl("stop slmodemd.service softmodem.service")
    machine.succeed("journalctl -u slmodemd -u softmodem --no-pager > /var/lib/softmodem/journal.txt")
    machine.execute("cd /var/lib/softmodem && sox -M *-answer-rx.wav *-answer-tx.wav ${wav}")
    machine.copy_from_machine("/var/lib/softmodem", "dumps")
    with open(os.path.join(os.environ["out"], "status"), "w") as file:
        file.write("ok\n" if status == 0 else output)
  '';
}
