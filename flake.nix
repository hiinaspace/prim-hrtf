{
  description = "prim-hrtf: baked-HRTF binaural renderer bench for Prim voice";
  # Same pin as prim-identity, so the store is shared.
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/dc5d91f840324650bac8c379428c7037a416959a";
  outputs = { self, nixpkgs }: let
    pkgs = import nixpkgs { system = "x86_64-linux"; };
    cross = pkgs.pkgsCross.aarch64-multiplatform;
  in {
    devShells.x86_64-linux = {
      default = pkgs.mkShell {
        packages = with pkgs; [ cargo rustc rustfmt clippy ffmpeg uv pre-commit ];
      };
      # cargo build --release --target aarch64-unknown-linux-gnu
      aarch64 = pkgs.mkShell {
        packages = [ pkgs.cargo cross.buildPackages.rustc cross.stdenv.cc pkgs.patchelf ];
        CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER = "${cross.stdenv.cc}/bin/aarch64-unknown-linux-gnu-cc";
      };
      # bench/mumble: builds S's Mumble HrtfSpatializer against libmysofa + Qt Core
      mumble = pkgs.mkShell {
        packages = with pkgs; [ gcc pkg-config libmysofa qt6.qtbase zlib ];
      };
    };
  };
}
