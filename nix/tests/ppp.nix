{ softmodem }:

let
  port = "/run/softmodem/ttyS0";
  # The default 3 s restart is shorter than one round trip at 300 bit/s.
  pppOptions = "nodetach local noauth nocrtscts noccp noipv6 mru 296 mtu 296 asyncmap 0 lcp-restart 15 ipcp-restart 15 debug";
in
{
  name = "softmodem-ppp";

  nodes = {
    isp =
      { pkgs, ... }:
      {
        networking.firewall.allowedUDPPorts = [ 5300 ];
        environment.systemPackages = [ softmodem ];

        systemd.services.softmodem = {
          wantedBy = [ "multi-user.target" ];
          serviceConfig = {
            ExecStart = "${softmodem}/bin/softmodem wire answer --local 0.0.0.0:5300 --pty ${port} --dump /var/lib/softmodem";
            RuntimeDirectory = "softmodem";
            StateDirectory = "softmodem";
          };
        };

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
      {
        environment.systemPackages = [ softmodem ];

        systemd.services.softmodem = {
          path = [
            pkgs.getent
            pkgs.coreutils
          ];
          script = ''
            isp=$(getent ahostsv4 isp | head -n 1 | cut -d ' ' -f 1)
            exec ${softmodem}/bin/softmodem wire originate --peer "$isp:5300" --pty ${port} --dump /var/lib/softmodem
          '';
          serviceConfig = {
            RuntimeDirectory = "softmodem";
            StateDirectory = "softmodem";
          };
        };

        systemd.services.pppd = {
          requires = [ "softmodem.service" ];
          after = [ "softmodem.service" ];
          serviceConfig.ExecStart = "${pkgs.ppp}/bin/pppd ${port} ${pppOptions} noipdefault";
        };
      };
  };

  testScript = ''
    start_all()
    isp.wait_for_unit("pppd.service")
    isp.wait_until_succeeds("test -L ${port}")

    caller.wait_for_unit("multi-user.target")
    caller.systemctl("start softmodem.service")
    caller.wait_until_succeeds("test -L ${port}")
    caller.systemctl("start pppd.service")

    caller.wait_until_succeeds("ip -4 addr show ppp0 | grep -q 10.32.0.2", timeout=180)
    caller.succeed("ping -c 1 -W 30 10.32.0.1")
    isp.succeed("ping -c 1 -W 30 10.32.0.2")

    caller.systemctl("stop pppd.service softmodem.service")
    isp.wait_until_succeeds("journalctl -u softmodem | grep -q 'far end hung up'")
    caller.copy_from_machine("/var/lib/softmodem", "caller")
    isp.copy_from_machine("/var/lib/softmodem", "isp")
  '';
}
