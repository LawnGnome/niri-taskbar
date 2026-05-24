{
  lib,
  stdenv,
  rustPlatform,
  pkg-config,
  gtk3,
  src,
}:

let
  drv = rustPlatform.buildRustPackage {
    pname = "niri-taskbar";
    version = (lib.importTOML ../Cargo.toml).package.version;

    inherit src;

    # Maintainer note: this hash must be updated whenever Cargo.lock changes.
    # Set it to lib.fakeHash, run `nix build`, then substitute the correct
    # hash from the error output.
    cargoHash = "sha256-Wbwp1U2oLur8tKNYyWFRj4XWigOhbxcWm6/RFk46TXY=";

    nativeBuildInputs = [ pkg-config ];
    buildInputs = [ gtk3 ];

    installPhase = ''
      runHook preInstall
      install -Dm755 target/${stdenv.hostPlatform.rust.rustcTargetSpec}/release/libniri_taskbar.so \
        $out/lib/libniri_taskbar.so
      runHook postInstall
    '';

    passthru.modulePath = "${drv}/lib/libniri_taskbar.so";

    meta = {
      description = "Niri taskbar module for Waybar";
      homepage = "https://github.com/LawnGnome/niri-taskbar";
      license = lib.licenses.mit;
      maintainers = [ ];
      platforms = lib.platforms.linux;
    };
  };
in
drv
