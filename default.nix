with import <nixpkgs> {};
stdenv.mkDerivation {
  name = "outagefs";
  buildInputs = [ pkg-config fuse ];
}
