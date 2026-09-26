{
  stdenv,
  pkg-config,
  nix-expr-c,
  nix-store-c,
  nix-util-c,
  nix-flake-c,
}:
stdenv.mkDerivation {
  name = "nix-pbt-server-capi";
  src = ./server.c;
  dontUnpack = true;
  nativeBuildInputs = [ pkg-config ];
  buildInputs = [
    nix-expr-c
    nix-store-c
    nix-util-c
    nix-flake-c
  ];
  buildPhase = ''
    $CC -O2 -Wall -Werror -o nix-pbt-server-capi $src \
      $(pkg-config --cflags --libs nix-expr-c nix-store-c nix-util-c nix-flake-c)
  '';
  installPhase = ''
    install -Dm755 nix-pbt-server-capi $out/bin/nix-pbt-server-capi
  '';
}
