{
  withGui ? true,
  lib,
  stdenv,
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
  librsvg,
  libicns,
  libplist,
}:

let
  inherit (stdenv.hostPlatform) isLinux isDarwin;
  # Only the GUI is an app; the headless daemon stays a bare CLI binary.
  withAppBundle = withGui && isDarwin;
  launch = if withGui then "gui" else "daemon";
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
  pname = "limen";
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
  cargoLock = {
    lockFile = ../../Cargo.lock;
    allowBuiltinFetchGit = true;
  };
  # The worker hosts each VST3 plugin in a process of its own, so a plugin
  # that crashes cannot take limen down; limen looks for it beside itself.
  cargoBuildFlags = [
    "-p"
    "limen"
    "-p"
    "limen-vst-worker"
  ];
  cargoTestFlags =
    if withGui then
      [ "--workspace" ]
    else
      [
        "-p"
        "limen-core"
        "-p"
        "limen-rig"
        "-p"
        "limen"
      ];
  buildFeatures = lib.optionals withGui [ "limen/gui" ];

  SSL_CERT_FILE = "${cacert}/etc/ssl/certs/ca-bundle.crt";

  nativeBuildInputs = [
    makeWrapper
  ]
  ++ lib.optionals (withGui && isLinux) [ pkg-config ]
  # Render the app icon from the SVG the Linux desktop entry already ships.
  ++ lib.optionals withAppBundle [
    librsvg
    libicns
  ];
  # GPUI on Linux links X11/freetype directly; on macOS it links the system
  # frameworks through the SDK and needs nothing here.
  buildInputs = lib.optionals (withGui && isLinux) [
    fontconfig
    freetype
    libxkbcommon
    libxcb
  ];
  # The portal tests run a private dbus-daemon; macOS has no portal tests.
  nativeCheckInputs = lib.optionals isLinux [ dbus ];
  # `plutil` lives outside the store, so the bundle's plist is parsed with
  # libplist's `plistutil` instead.
  nativeInstallCheckInputs =
    lib.optionals isLinux [ desktop-file-utils ] ++ lib.optionals withAppBundle [ libplist ];

  # The GlobalShortcuts portal registers the app only with a desktop entry of
  # its ID, and GNOME Settings lists the app's shortcuts under it. macOS has
  # no desktop-entry convention; the daemon registers its hotkeys itself.
  postInstall =
    lib.optionalString isLinux ''
      install -Dm644 data/io.github.Emin017.Limen.desktop -t $out/share/applications
      install -Dm644 data/io.github.Emin017.Limen.svg -t $out/share/icons/hicolor/scalable/apps
      substituteInPlace $out/share/applications/io.github.Emin017.Limen.desktop \
        --replace-fail "Exec=limen daemon" "Exec=$out/bin/limen ${launch}"
    ''
    # macOS grants microphone and Accessibility access to a bundle only when
    # the running binary is the bundle's own executable, so the real Mach-O
    # moves into Limen.app and bin/limen becomes the symlink, not the reverse.
    # This runs before fixup, which still strips and ad-hoc signs the binary.
    + lib.optionalString withAppBundle ''
      contents=$out/Applications/Limen.app/Contents
      install -Dm644 data/Info.plist -t $contents
      substituteInPlace $contents/Info.plist \
        --replace-fail "@version@" "$version" \
        --replace-fail "@minimumSystemVersion@" "${stdenv.hostPlatform.darwinMinVersion}"

      mkdir -p $contents/MacOS $contents/Resources
      mv $out/bin/limen $contents/MacOS/limen
      ln -s $contents/MacOS/limen $out/bin/limen
      # Plugins load into the worker, not into the app: if the app is ever
      # notarized with the hardened runtime, it is the worker that needs the
      # com.apple.security.cs.disable-library-validation entitlement.
      mv $out/bin/limen-vst-worker $contents/MacOS/limen-vst-worker
      ln -s $contents/MacOS/limen-vst-worker $out/bin/limen-vst-worker

      # png2icns accepts only the sizes icns has a slot for; 64 is not one.
      iconset=$(mktemp -d)
      for size in 16 32 48 128 256 512 1024; do
        rsvg-convert --width $size --height $size \
          data/io.github.Emin017.Limen.svg --output $iconset/$size.png
      done
      png2icns $contents/Resources/Limen.icns $iconset/*.png
    '';

  doInstallCheck = true;
  installCheckPhase =
    if isLinux then
      ''
        runHook preInstallCheck
        entry=$out/share/applications/io.github.Emin017.Limen.desktop
        desktop-file-validate "$entry"
        grep -qx "Exec=$out/bin/limen ${launch}" "$entry"
        test -f $out/share/icons/hicolor/scalable/apps/io.github.Emin017.Limen.svg
        test -x $out/bin/limen-vst-worker
        runHook postInstallCheck
      ''
    else
      ''
        runHook preInstallCheck
        $out/bin/limen --help >/dev/null
        test -x $out/bin/limen-vst-worker
      ''
      + lib.optionalString withAppBundle ''
        contents=$out/Applications/Limen.app/Contents
        test -f $contents/MacOS/limen
        test -x $contents/MacOS/limen-vst-worker
        test ! -L $contents/MacOS/limen
        test -x $contents/MacOS/limen
        test "$(realpath $out/bin/limen)" = "$(realpath $contents/MacOS/limen)"
        plistutil --infile $contents/Info.plist --outfile /dev/null
        ! grep -q '@[A-Za-z]*@' $contents/Info.plist
        test -s $contents/Resources/Limen.icns
      ''
      + ''
        runHook postInstallCheck
      '';

  # Linux runtime deps are external binaries provided on PATH; the macOS
  # backends (cpal/enigo/osascript) are linked or already on the system PATH.
  postFixup = lib.optionalString isLinux ''
    wrapProgram $out/bin/limen \
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
    description = "System-wide whole-take voice input";
    homepage = "https://github.com/Emin017/limen";
    mainProgram = "limen";
    platforms = lib.platforms.linux ++ lib.platforms.darwin;
  };
}
