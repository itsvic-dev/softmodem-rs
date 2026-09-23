{
  description = "softmodem: a V.21, V.22 and V.22bis modem that places real calls over SIP";

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
        }
        // nixpkgs.lib.optionalAttrs pkgs.stdenv.isLinux {
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
          ppp-automode = pkgs.testers.runNixOSTest (
            import ./nix/tests/ppp.nix {
              inherit softmodem;
              label = "automode";
              modulation = "";
            }
          );
          cuse = pkgs.testers.runNixOSTest (import ./nix/tests/cuse.nix { inherit pkgs softmodem; });
        }
      );

      devShells = forSystems (pkgs: {
        default = pkgs.mkShell {
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
              spandsp3
            ]
            ++ lib.optional stdenv.hostPlatform.isLinux alsa-lib;
        };
      });

      formatter = forSystems (pkgs: pkgs.nixfmt);
    };
}
