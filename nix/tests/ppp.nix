# SPDX-FileCopyrightText: 2026 Wiktor Bryk <contact@itsvic.dev>
#
# SPDX-License-Identifier: GPL-3.0-or-later

{
  softmodem,
  label ? "V21",
  modulation ? "+MS=V21,0",
}:

let
  port = "/run/softmodem/ttyS0";
  # The default 3 s restart is shorter than one round trip at 300 bit/s.
  pppOptions = "nodetach local noauth nocrtscts noccp noipv6 mru 296 mtu 296 asyncmap 0 lcp-restart 15 ipcp-restart 15 debug";

  modemService = extraArgs: {
    wantedBy = [ "multi-user.target" ];
    serviceConfig = {
      ExecStart = "${softmodem}/bin/softmodem wire --local 0.0.0.0:5300 --pty ${port} --dump /var/lib/softmodem ${extraArgs}";
      RuntimeDirectory = "softmodem";
      StateDirectory = "softmodem";
    };
  };
in
{
  name = "softmodem-ppp-${label}";

  nodes = {
    isp =
      { pkgs, ... }:
      {
        networking.firewall.allowedUDPPorts = [ 5300 ];
        # dhcpcd would add ppp0's address again with the peer as its broadcast.
        networking.dhcpcd.denyInterfaces = [ "ppp*" ];
        environment.systemPackages = [ softmodem ];

        # Quiet, so that RING and CONNECT do not reach pppd as line noise.
        systemd.services.softmodem = modemService "--init ATE0Q1S0=1${modulation}";

        systemd.services.pppd = {
          wantedBy = [ "multi-user.target" ];
          requires = [ "softmodem.service" ];
          after = [ "softmodem.service" ];
          serviceConfig = {
            ExecStart = "${pkgs.ppp}/bin/pppd ${port} ${pppOptions} persist silent lcp-echo-interval 10 lcp-echo-failure 3 10.32.0.1:10.32.0.2";
            Restart = "always";
            RestartSec = 2;
          };
        };
      };

    caller =
      { pkgs, ... }:
      let
        chat = "${pkgs.ppp}/bin/chat";
      in
      {
        networking.firewall.allowedUDPPorts = [ 5300 ];
        networking.dhcpcd.denyInterfaces = [ "ppp*" ];
        environment.systemPackages = [ softmodem ];

        systemd.services.softmodem = modemService "" // {
          path = [
            pkgs.getent
            pkgs.coreutils
          ];
          script = ''
            until isp=$(getent ahostsv4 isp | head -n 1 | cut -d ' ' -f 1) && [ -n "$isp" ]; do
              sleep 1
            done
            exec ${softmodem}/bin/softmodem wire --local 0.0.0.0:5300 --peer "$isp:5300" --pty ${port} --dump /var/lib/softmodem --init 'AT&C1${modulation}'
          '';
          serviceConfig = {
            RuntimeDirectory = "softmodem";
            StateDirectory = "softmodem";
          };
        };

        systemd.services.pppd = {
          requires = [ "softmodem.service" ];
          after = [ "softmodem.service" ];
          serviceConfig.ExecStart = pkgs.writeShellScript "dial-isp" ''
            exec ${pkgs.ppp}/bin/pppd ${port} ${pppOptions} noipdefault \
              connect "${chat} -v -t 60 ''' ATZ OK ATDT0300 CONNECT '\c'" \
              disconnect "${chat} -v ''' '\d\d+++\d\d\c' OK ATH0 OK"
          '';
        };
      };
  };

  testScript = ''
    start_all()
    isp.wait_for_unit("pppd.service")
    isp.wait_until_succeeds("test -L ${port}")
    caller.wait_for_unit("softmodem.service")
    caller.wait_until_succeeds("test -L ${port}")

    import time

    def hangups():
        return int(isp.succeed("journalctl -u pppd | grep -c 'Modem hangup' || true").strip())

    for attempt in (1, 2):
        with subtest(f"call {attempt}"):
            before = hangups()
            dialled = time.monotonic()
            caller.systemctl("start pppd.service")
            caller.wait_until_succeeds("ip -4 addr show ppp0 | grep -q 10.32.0.2", timeout=180)
            print(f"call {attempt}: address after {time.monotonic() - dialled:.1f} s from dialling")
            caller.succeed("ping -c 1 -W 30 10.32.0.1")
            isp.succeed("ping -c 1 -W 30 10.32.0.2")
            caller.systemctl("stop pppd.service")
            for machine in (caller, isp):
                machine.wait_until_succeeds(f"journalctl -u softmodem | grep -c 'call ended' | grep -qx {attempt}", timeout=90)
            isp.wait_until_succeeds(f"[ $(journalctl -u pppd | grep -c 'Modem hangup') -gt {before} ]", timeout=30)

    caller.systemctl("stop softmodem.service")
    isp.systemctl("stop softmodem.service")
    caller.copy_from_machine("/var/lib/softmodem", "caller")
    isp.copy_from_machine("/var/lib/softmodem", "isp")
  '';
}
