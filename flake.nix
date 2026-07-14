{
  description = "CodexBar-Linux dev environment — GTK4/SNI tray app for AI provider usage";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-26.05";

  outputs =
    { self, nixpkgs }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
      nativeDeps =
        pkgs: with pkgs; [
          pkg-config
          wrapGAppsHook4
        ];
      buildDeps =
        pkgs: with pkgs; [
          gtk4
          gtk4-layer-shell
          glib
          cairo
          pango
          gdk-pixbuf
          graphene
        ];
    in
    {
      devShells = forAllSystems (pkgs: {
        default = pkgs.mkShell {
          packages =
            with pkgs;
            [
              rustc
              cargo
              clippy
              rustfmt
              rust-analyzer
              just
            ]
            ++ nativeDeps pkgs
            ++ buildDeps pkgs;
          shellHook = ''echo "dev shell ready — run \`just\`"'';
        };
      });

      packages = forAllSystems (pkgs: rec {
        codexbar = pkgs.rustPlatform.buildRustPackage {
          pname = "codexbar";
          version = "26.7.0";
          src = self;
          cargoLock.lockFile = ./Cargo.lock;
          nativeBuildInputs = nativeDeps pkgs;
          buildInputs = buildDeps pkgs;
          meta = {
            description = "System tray usage monitor for AI coding providers";
            license = pkgs.lib.licenses.mit;
            mainProgram = "codexbar";
          };
        };
        default = codexbar;
      });

      formatter = forAllSystems (pkgs: pkgs.nixfmt);
    };
}
