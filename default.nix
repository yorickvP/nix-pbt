# Evaluation servers for the nix-pbt server protocol.
#
#   nix-build -A server-capi                          # against nixpkgs' Nix
#   nix-build -A server-capi --arg nixFlake ../nix    # against a Nix checkout
#   nix-build -A fix                                  # fix main, with --pbt-server
#   nix-build -A fix --arg fixSrc ../fix              # a fix checkout
{
  nixFlake ? null,
  fixSrc ? builtins.fetchGit {
    url = "https://github.com/psyclyx/fix";
    ref = "main";
  },
}:
let
  flake = builtins.getFlake (toString nixFlake);
  pkgs =
    if nixFlake == null then
      import <nixpkgs> { }
    else
      flake.inputs.nixpkgs.legacyPackages.${builtins.currentSystem};
  nixLibs =
    if nixFlake == null then
      pkgs.nixVersions.latest.libs
    else
      flake.packages.${builtins.currentSystem};
in
{
  server-capi = pkgs.callPackage ./servers/nix-capi/package.nix {
    inherit (nixLibs) nix-expr-c nix-store-c nix-util-c nix-flake-c;
  };

  # fix with `fix repl --pbt-server`, which speaks the server protocol.
  fix = (import fixSrc { }).fix.overrideAttrs (old: {
    patches = (old.patches or [ ]) ++ [ ./servers/fix/pbt-server.patch ];
  });
}
