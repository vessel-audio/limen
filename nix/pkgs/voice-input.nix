{
  lib,
  rustPlatform,
  makeWrapper,
  dbus,
  desktop-file-utils,
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
  # The portal tests run a private dbus-daemon.
  nativeCheckInputs = [ dbus ];
  nativeInstallCheckInputs = [ desktop-file-utils ];

  # The GlobalShortcuts portal registers the app only with a desktop entry of
  # its ID, and GNOME Settings lists the app's shortcuts under it.
  postInstall = ''
    install -Dm644 data/io.github.Emin017.VoiceInput.desktop -t $out/share/applications
    substituteInPlace $out/share/applications/io.github.Emin017.VoiceInput.desktop \
      --replace-fail "Exec=voice-input daemon" "Exec=$out/bin/voice-input daemon"
  '';

  doInstallCheck = true;
  installCheckPhase = ''
    runHook preInstallCheck
    entry=$out/share/applications/io.github.Emin017.VoiceInput.desktop
    desktop-file-validate "$entry"
    grep -qx "Exec=$out/bin/voice-input daemon" "$entry"
    runHook postInstallCheck
  '';

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
