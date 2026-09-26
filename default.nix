# Evaluation servers for the nix-pbt server protocol.
#
#   nix-build -A server-capi                          # against nixpkgs' Nix
#   nix-build -A server-capi --arg nixFlake ../nix    # against a Nix checkout
{
  nixFlake ? null,
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
}
