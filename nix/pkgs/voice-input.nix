{
  lib,
  rustPlatform,
  makeWrapper,
  pipewire,
  libnotify,
  cacert,
}:

rustPlatform.buildRustPackage {
  pname = "voice-input";
  version = "0.1.0";

  src = lib.cleanSourceWith {
    src = ../../.;
    filter =
      path: type:
      let
        name = builtins.baseNameOf path;
      in
      !(builtins.elem name [
        "target"
        "result"
      ])
      && lib.cleanSourceFilter path type;
  };
  cargoLock.lockFile = ../../Cargo.lock;

  SSL_CERT_FILE = "${cacert}/etc/ssl/certs/ca-bundle.crt";

  nativeBuildInputs = [ makeWrapper ];

  postFixup = ''
    wrapProgram $out/bin/voice-input \
      --prefix PATH : ${
        lib.makeBinPath [
          pipewire
          libnotify
        ]
      }
  '';

  meta = {
    description = "System-wide whole-take voice input for Wayland";
    homepage = "https://github.com/Emin017/voice-input";
    mainProgram = "voice-input";
    platforms = lib.platforms.linux;
  };
}
