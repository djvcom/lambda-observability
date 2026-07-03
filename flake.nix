{
  description = "Lambda Observability - OpenTelemetry tooling for AWS Lambda";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    { nixpkgs, rust-overlay, ... }:
    let
      forAllSystems =
        fn:
        nixpkgs.lib.genAttrs nixpkgs.lib.systems.flakeExposed (
          system:
          fn (
            import nixpkgs {
              inherit system;
              overlays = [ rust-overlay.overlays.default ];
            }
          )
        );
    in
    {
      devShells = forAllSystems (pkgs: {
        default = pkgs.mkShell {
          packages = with pkgs; [
            (rust-bin.stable.latest.default.override {
              extensions = [
                "rust-src"
                "rust-analyzer"
              ];
            })
            cargo-lambda

            openssl
            pkg-config

            nodejs
            python3

            nixfmt
            statix
            deadnix
          ];

          env = {
            OPENSSL_DIR = "${pkgs.openssl.dev}";
            OPENSSL_LIB_DIR = "${pkgs.openssl.out}/lib";
          };
        };
      });

      formatter = forAllSystems (pkgs: pkgs.nixfmt-rfc-style);
    };
}
