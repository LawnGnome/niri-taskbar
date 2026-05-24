{
  description = "Niri taskbar module for Waybar";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { self, nixpkgs }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      forEachSystem = nixpkgs.lib.genAttrs systems;
    in
    {
      packages = forEachSystem (system: {
        default = nixpkgs.legacyPackages.${system}.callPackage ./nix/package.nix {
          src = self;
        };
      });

      overlays.default = final: _: {
        niri-taskbar = final.callPackage ./nix/package.nix { src = self; };
      };

      homeManagerModules.default =
        { pkgs, lib, config, ... }:
        let
          cfg = config.programs.niri-taskbar;
        in
        {
          options.programs.niri-taskbar.enable = lib.mkEnableOption "niri-taskbar Waybar module";

          config = lib.mkIf cfg.enable {
            home.file.".local/lib/libniri_taskbar.so".source =
              self.packages.${pkgs.system}.default.modulePath;
          };
        };
    };
}
