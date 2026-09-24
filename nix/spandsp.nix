# spandsp's last release, with its V.34 modem, as a second interop target.
{ spandsp3, fetchFromGitHub }:

spandsp3.overrideAttrs (previousAttrs: {
  version = "3.1.1";
  src = fetchFromGitHub {
    owner = "freeswitch";
    repo = "spandsp";
    tag = "v3.1.1";
    hash = "sha256-RTJNWuKAXAxRnMp14t9TXQombXqwvSplstBJ+OlzA9w=";
  };
  configureFlags = previousAttrs.configureFlags ++ [ "--enable-v34" ];
  # data_modems.c reaches these without the Godard headers they need.
  postPatch = previousAttrs.postPatch + ''
    for rx in src/spandsp/private/v17rx.h src/spandsp/private/v29rx.h; do
      substituteInPlace $rx --replace-fail '#define _SPANDSP_PRIVATE_V' \
        $'#include <spandsp/godard.h>\n#include <spandsp/private/godard.h>\n#define _SPANDSP_PRIVATE_V'
    done
  ''
  # d9681c3 does not step past the XID's optional functions.
  + ''
    substituteInPlace src/v42.c --replace-fail \
      'put_net_unaligned_uint32(buf, 0x8A890000);' \
      'put_net_unaligned_uint32(buf, 0x8A890000); buf += 4;'
  '';
  preConfigure = ''
    autoreconf -fi
  '';
  # telephony.h uses typeof, which clang's -std=c99 does not know.
  env.NIX_CFLAGS_COMPILE = previousAttrs.env.NIX_CFLAGS_COMPILE + " -Dtypeof=__typeof__";
  doCheck = false;
})
