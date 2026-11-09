{
  lib,
  rustPlatform,
}:
let
  cargoToml = lib.importTOML ../Cargo.toml;
in
rustPlatform.buildRustPackage {
  pname = "kojutsu";
  inherit (cargoToml.package) version;

  src = lib.fileset.toSource {
    root = ../.;
    fileset = lib.fileset.unions [
      ../Cargo.toml
      ../Cargo.lock
      ../build.rs
      ../src
      ../vendored
    ];
  };
  cargoLock.lockFile = ../Cargo.lock;

  meta = {
    description = "Terminal UI for Jujutsu";
    homepage = "https://github.com/kosayoda/kojutsu";
    license = with lib.licenses; [
      mit
      asl20
    ];
    mainProgram = "kojutsu";
    platforms = lib.platforms.linux;
  };
}
