{ pkgs, softmodem }:

let
  isp_port = "/run/softmodem/ttyS0";

  modemService = peer: args: {
    wantedBy = [ "multi-user.target" ];
    path = [
      pkgs.getent
      pkgs.coreutils
    ];
    script = ''
      until address=$(getent ahostsv4 ${peer} | head -n 1 | cut -d ' ' -f 1) && [ -n "$address" ]; do
        sleep 1
      done
      exec ${softmodem}/bin/softmodem wire --local 0.0.0.0:5300 --peer "$address:5300" ${args}
    '';
    serviceConfig.RuntimeDirectory = "softmodem";
  };

  guestKernel = pkgs.linuxPackages.kernel;
  guestMachine =
    {
      aarch64-linux = {
        qemu = "qemu-system-aarch64 -M virt -cpu cortex-a57 -accel tcg,thread=multi";
        image = "Image";
        console = "ttyAMA0";
        modules = [ ];
      };
      x86_64-linux = {
        qemu = "qemu-system-x86_64 -M microvm,pcie=on -cpu max -accel kvm -accel tcg";
        image = "bzImage";
        console = "ttyS0";
        modules = [ "kernel/drivers/tty/serial/8250/8250_pci.ko.xz" ];
      };
    }
    .${pkgs.stdenv.hostPlatform.system};
  guestModules = pkgs.runCommand "guest-modules" { nativeBuildInputs = [ pkgs.xz ]; } ''
    mkdir $out
    for module in ${toString guestMachine.modules}; do
      xz -dc ${guestKernel.modules}/lib/modules/${guestKernel.modDirVersion}/$module > $out/$(basename $module .xz)
    done
  '';
  guestInitrd = pkgs.makeInitrd {
    contents = [
      {
        object = pkgs.writeScript "init" (builtins.readFile ./guest-init.sh);
        symlink = "/init";
      }
      {
        object = "${pkgs.pkgsStatic.busybox}/bin";
        symlink = "/bin";
      }
      {
        object = guestModules;
        symlink = "/modules";
      }
    ];
  };
  # The console goes to a file: on the test driver's terminal, QEMU is stopped by SIGTTOU.
  guest = pkgs.writeShellScript "run-guest" ''
    ${pkgs.qemu_test}/bin/${guestMachine.qemu} \
      -smp 2 -m 256 -display none -monitor none -no-reboot \
      -serial file:/tmp/guest-console.log \
      -kernel ${guestKernel}/${guestMachine.image} -initrd ${guestInitrd}/initrd \
      -append "console=${guestMachine.console} panic=-1 quiet" \
      -chardev serial,id=modem,path=/dev/ttySM0 -device pci-serial,chardev=modem \
      < /dev/null
    status=$?
    cat /tmp/guest-console.log
    exit $status
  '';
in
{
  name = "softmodem-cuse";

  nodes = {
    isp = {
      networking.firewall.allowedUDPPorts = [ 5300 ];
      systemd.services.softmodem = modemService "caller" "--pty ${isp_port} --init ATS0=1";
    };

    caller = {
      networking.firewall.allowedUDPPorts = [ 5300 ];
      boot.kernelModules = [ "cuse" ];
      virtualisation.memorySize = 2048;
      environment.systemPackages = [ pkgs.python3 ];
      systemd.services.softmodem = modemService "isp" "--cuse ttySM0 --init 'AT&C1'";
    };
  };

  testScript = ''
    start_all()
    isp.wait_until_succeeds("test -L ${isp_port}")
    caller.wait_for_unit("softmodem.service")
    caller.wait_until_succeeds("test -c /dev/ttySM0")

    with subtest("DCD follows a call the computer dials"):
        print(caller.succeed("python3 ${./serial-probe.py} dial"))

    with subtest("RI shows an incoming call, DCD the answered one"):
        isp.succeed("printf 'ATDT1\\r' > ${isp_port}")
        print(caller.succeed("python3 ${./serial-probe.py} answer"))

    with subtest("a QEMU guest sees DCD through its emulated 16550"):
        status, output = caller.execute("timeout 600 ${guest} 2>&1")
        output = output.replace("\r", "")
        for line in output.splitlines():
            if line.startswith("guest"):
                caller.log(line)
        assert status == 0, f"the guest did not finish, status {status}"

        def flags(stage):
            line = next(l for l in output.splitlines() if l.startswith(f"guest {stage}:"))
            return line.rsplit(" ", 1)[-1].split("|")

        assert "guest: CONNECT" in output, "the guest never connected"
        assert "CD" not in flags("before dialling"), "DCD on before the guest dialled"
        assert "CD" in flags("connected"), "the guest saw no DCD while connected"
        assert "CD" not in flags("hung up"), "DCD stayed on after the guest hung up"
  '';
}
