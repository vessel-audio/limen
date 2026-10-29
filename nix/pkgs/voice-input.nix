{
  withGui ? true,
  lib,
  rustPlatform,
  makeWrapper,
  dbus,
  desktop-file-utils,
  pipewire,
  libnotify,
  cacert,
  pkg-config,
  fontconfig,
  freetype,
  libxkbcommon,
  libxcb,
  wayland,
  vulkan-loader,
  libGL,
}:

let
  runtimePipewire =
    if withGui then
      pipewire
    else
      (pipewire.override {
        vulkanSupport = false;
        x11Support = false;
      }).overrideAttrs
        (previous: {
          mesonFlags = previous.mesonFlags ++ [
            (lib.mesonEnable "ffmpeg" false)
            (lib.mesonEnable "pw-cat-ffmpeg" false)
            (lib.mesonEnable "gstreamer" false)
            (lib.mesonEnable "gstreamer-device-provider" false)
            (lib.mesonEnable "libcamera" false)
          ];
        });
in
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
  cargoBuildFlags = [
    "-p"
    "voice-input"
  ];
  cargoTestFlags =
    if withGui then
      [ "--workspace" ]
    else
      [
        "-p"
        "voice-input-core"
        "-p"
        "voice-input"
      ];
  buildFeatures = lib.optionals withGui [ "voice-input/gui" ];

  SSL_CERT_FILE = "${cacert}/etc/ssl/certs/ca-bundle.crt";

  nativeBuildInputs = [ makeWrapper ] ++ lib.optionals withGui [ pkg-config ];
  buildInputs = lib.optionals withGui [
    fontconfig
    freetype
    libxkbcommon
    libxcb
  ];
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
          runtimePipewire
          libnotify
        ]
      }       ${lib.optionalString withGui "--prefix LD_LIBRARY_PATH : ${
        lib.makeLibraryPath [
          wayland
          vulkan-loader
          libGL
        ]
      }"}
  '';

  meta = {
    description = "System-wide whole-take voice input for Wayland";
    homepage = "https://github.com/Emin017/voice-input";
    mainProgram = "voice-input";
    platforms = lib.platforms.linux;
  };
}
