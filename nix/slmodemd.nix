# slmodemd from Aon's D-Modem, a V.22bis and V.34 peer for checks only.
{
  lib,
  stdenv,
  fetchFromGitHub,
}:

stdenv.mkDerivation {
  pname = "slmodemd";
  version = "0-unstable-2021-10-28";

  src = fetchFromGitHub {
    owner = "strozfriedberg";
    repo = "D-Modem";
    rev = "636959b";
    hash = "sha256-kZCbpGDzjV5dHoVM4hQeIYxhg1cklnU8TOQwrlWH05E=";
  };

  sourceRoot = "source/slmodemd";

  makeFlags = [ "CC=${stdenv.cc.targetPrefix}cc" ];
  buildFlags = [ "slmodemd" ];

  installPhase = ''
    runHook preInstall
    install -Dm755 slmodemd $out/bin/slmodemd
    runHook postInstall
  '';

  meta = {
    description = "Smart Link soft modem daemon with D-Modem's socket driver";
    homepage = "https://github.com/strozfriedberg/D-Modem";
    license = lib.licenses.gpl2Only;
    # dsplibs.o, the Smart Link DSP, is a 32-bit x86 object.
    platforms = [ "i686-linux" ];
  };
}
