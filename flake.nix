# SPDX-FileCopyrightText: 2026 Wiktor Bryk <contact@itsvic.dev>
#
# SPDX-License-Identifier: GPL-3.0-or-later

{
  description = "softmodem: a software modem that places real calls over SIP";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { self, nixpkgs }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "x86_64-darwin"
        "aarch64-darwin"
      ];
      forSystems = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
    in
    {
      overlays.default = final: _prev: {
        softmodem = final.callPackage ./nix/softmodem.nix { };
      };

      packages = forSystems (pkgs: rec {
        softmodem = pkgs.callPackage ./nix/softmodem.nix { };
        default = softmodem;
      });

      checks = forSystems (
        pkgs:
        let
          softmodem = pkgs.callPackage ./nix/softmodem.nix { };
        in
        {
          reuse = pkgs.runCommand "softmodem-reuse" { nativeBuildInputs = [ pkgs.reuse ]; } ''
            reuse --root ${self} lint
            touch $out
          '';
          fmt =
            pkgs.runCommand "softmodem-fmt"
              {
                nativeBuildInputs = [
                  pkgs.cargo
                  pkgs.rustfmt
                ];
              }
              ''
                cd ${self}
                cargo fmt --all --check
                touch $out
              '';
        }
        // nixpkgs.lib.optionalAttrs pkgs.stdenv.hostPlatform.isLinux {
          ppp = pkgs.testers.runNixOSTest (import ./nix/tests/ppp.nix { inherit softmodem; });
          ppp-v22 = pkgs.testers.runNixOSTest (
            import ./nix/tests/ppp.nix {
              inherit softmodem;
              label = "V22";
              modulation = "+MS=V22,0";
            }
          );
          ppp-v22bis = pkgs.testers.runNixOSTest (
            import ./nix/tests/ppp.nix {
              inherit softmodem;
              label = "V22B";
              modulation = "+MS=V22B,0";
            }
          );
          ppp-v90 = pkgs.testers.runNixOSTest (
            import ./nix/tests/ppp.nix {
              inherit softmodem;
              label = "V90";
              modulation = "+MS=V90,0";
            }
          );
          ppp-v90-stalls = pkgs.testers.runNixOSTest (
            import ./nix/tests/ppp.nix {
              inherit softmodem;
              label = "V90-stalls";
              modulation = "+MS=V90,0";
              impairment = "--stall 0.005 --stall-for 500";
            }
          );
          # A VoIP provider's path, as real calls met it: see "Wire" in docs/transport.md.
          ppp-v90-voip = pkgs.testers.runNixOSTest (
            import ./nix/tests/ppp.nix {
              inherit softmodem;
              label = "V90-voip";
              modulation = "+MS=V90,0";
              impairment = "--delay 140 --conceal 0.002 --seed 1";
              ispImpairment = "--gateway-after 300 --gateway-noise 100";
            }
          );
          ppp-automode = pkgs.testers.runNixOSTest (
            import ./nix/tests/ppp.nix {
              inherit softmodem;
              label = "automode";
              modulation = "";
            }
          );
          cuse = pkgs.testers.runNixOSTest (import ./nix/tests/cuse.nix { inherit pkgs softmodem; });
        }
        // nixpkgs.lib.optionalAttrs pkgs.stdenv.hostPlatform.isLinux (
          let
            guestPkgs = nixpkgs.legacyPackages.x86_64-linux;
            peer =
              { label, ... }@call:
              let
                run = pkgs.testers.runNixOSTest (
                  import ./nix/tests/slmodemd.nix (
                    call
                    // {
                      inherit guestPkgs;
                      softmodem = guestPkgs.callPackage ./nix/softmodem.nix { };
                      bridge = (import ./nix/Cargo.nix { pkgs = guestPkgs; }).workspaceMembers.softmodem-slmodem.build;
                      slmodemd = pkgs.pkgsCross.gnu32.callPackage ./nix/slmodemd.nix { };
                    }
                  )
                );
              in
              # The run always succeeds, to keep a failed call's recording in `passthru.run`.
              pkgs.runCommand "softmodem-slmodemd-${label}-passed" { passthru = { inherit run; }; } ''
                cat ${run}/status
                grep -qx ok ${run}/status
                ln -s ${run} $out
              '';
          in
          {
            slmodemd-v22bis = peer {
              label = "V22B";
              ours = "+MS=V22B,0";
              theirs = "+MS=122,0";
            };
            slmodemd-v34 = peer {
              label = "V34";
              ours = "+MS=V34,0";
              theirs = "+MS=34,0,2400,33600";
            };
            slmodemd-v90 = peer {
              label = "V90";
              ours = "+MS=V90,0";
              theirs = "+MS=90";
            };
            slmodemd-v90-renegotiate = peer {
              label = "V90-renegotiate";
              ours = "+MS=V90,0";
              theirs = "+MS=90";
              noise = {
                after = 40;
                rms = 10;
                later = 30;
                way = "to-slmodemd";
              };
            };
            # slmodemd, as the analogue modem, on a VoIP provider's path like ppp-v90-voip's.
            slmodemd-v90-voip = peer {
              label = "V90-voip";
              ours = "+MS=V90,0";
              theirs = "+MS=90";
              impairment = "--delay 140 --conceal 0.002 --seed 1 --gateway-after 300 --gateway-noise 100";
            };
            slmodemd-v34-retrain = peer {
              label = "V34-retrain";
              ours = "+MS=V34,0";
              theirs = "+MS=34,0,2400,33600";
              retrain = true;
            };
            # Noise about 31 dB below the signal, once in data mode, for slmodemd to retrain.
            slmodemd-v34-noise = peer {
              label = "V34-noise";
              ours = "+MS=V34,0";
              theirs = "+MS=34,0,2400,33600";
              noise = {
                after = 20;
                rms = 95;
                later = 15;
              };
            };
            # Noise about 32 dB down toward the softmodem only, for it to renegotiate.
            slmodemd-v34-renegotiate = peer {
              label = "V34-renegotiate";
              ours = "+MS=V34,0";
              theirs = "+MS=34,0,2400,33600";
              noise = {
                after = 20;
                rms = 85;
                later = 15;
                way = "to-softmodem";
              };
            };
          }
        )
      );

      devShells = forSystems (
        pkgs:
        let
          shell =
            spandsp:
            pkgs.mkShell {
              packages =
                with pkgs;
                [
                  cargo
                  rustc
                  clippy
                  rustfmt
                  rust-analyzer
                  crate2nix
                  # Only for softmodem-interop, which tests against spandsp.
                  pkg-config
                  spandsp
                ]
                ++ lib.optional stdenv.hostPlatform.isLinux alsa-lib;
            };
        in
        {
          default = shell pkgs.spandsp3;
          spandsp-3_1 = shell (pkgs.callPackage ./nix/spandsp.nix { });
        }
      );

      formatter = forSystems (pkgs: pkgs.nixfmt);

      # The aarch64 checks run the x86-64 slmodemd guest under TCG, too slow for V.34 in real time.
      hydraJobs.checks = { inherit (self.checks) x86_64-linux; };
    };
}
