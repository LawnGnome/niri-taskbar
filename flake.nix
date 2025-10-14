{
    description = "Niri taskbar module for Waybar";
    inputs = {
        nixpkgs.url = "github:nixos/nixpkgs/nixos-unstable";
    };
    outputs = { self, nixpkgs }:
      let
        supportedSystems = [ 
          "x86_64-linux"
        ];
      
        forAllSystems = nixpkgs.lib.genAttrs supportedSystems;

        pkgsForSystem = system: (import nixpkgs {
            inherit system;
            overlays = [ self.overlays.default ];
          });
      in
      {
        overlays.default = final: _prev:
        let
          cargo_package = (builtins.fromTOML (builtins.readFile ./Cargo.toml)).package;
          inherit (final) pkg-config gtk3 at-spi2-atk pango cairo glib gdk-pixbuf libpeas2 lib rustPlatform;
        in
        {
            niri-taskbar = rustPlatform.buildRustPackage rec {
              name = "niri-taskbar";
              version = "0.3.0+niri.25.08"; # Todo get from toml?

              src = lib.cleanSource ./.;
              
              cargoLock.lockFile = ./Cargo.lock;

              nativeBuildInputs = [
                pkg-config
              ];

              buildInputs = [
                gtk3
                at-spi2-atk
                pango
                cairo
                glib
                gdk-pixbuf
                libpeas2
              ];
            };

          meta = {
              description = cargo_package.description;
              homepage = cargo_package.repository;
              license = lib.licenses.mit;
              platforms = lib.platforms.linux;
              maintainers = with lib.maintainers; [ LawnGnome ];
          };
        };

        packages = forAllSystems (system: rec {
            inherit (pkgsForSystem system) niri-taskbar;
            default = niri-taskbar;
        });
      };
  }
