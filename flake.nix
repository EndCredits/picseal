{
  description = "Picseal - dev shell (Node 22 + Rust + wasm-pack)";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";

  outputs =
    { self, nixpkgs }:
    let
      systems = [
        "aarch64-darwin"
        "x86_64-darwin"
        "aarch64-linux"
        "x86_64-linux"
      ];
      eachSystem =
        f:
        nixpkgs.lib.genAttrs systems (
          system: f (import nixpkgs { inherit system; })
        );
    in
    {
      devShells = eachSystem (
        pkgs: {
          default = pkgs.mkShell {
            packages = with pkgs; [
              nodejs_22
              cargo
              rustc
              rustfmt
              clippy
              wasm-pack
              lld
              git
            ];

            shellHook = ''
              echo "Picseal dev shell"
              echo "  node:       $(node -v)"
              echo "  npm:        $(npm -v)"
              echo "  rustc:      $(rustc -vV | sed -n 's/^release: //p')"
              echo "  wasm-pack:  $(wasm-pack -V)"
              if [ ! -d src/wasm ]; then
                echo ""
                echo "First-time setup: npm run build:wasm && npm install"
              fi
            '';
          };
        }
      );
    };
}
