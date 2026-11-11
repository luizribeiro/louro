# Terminal demos, recorded inside the VM tests in Luiz's terminal look: the
# recorder is copied from snowglobe's nix/demos.
{ pkgs }:
let
  lookFile = ./terminal-look.json;
in
{
  recorder = pkgs.writeShellScriptBin "record" ''
    export TERMINAL_LOOK=${lookFile}
    exec ${
      pkgs.writers.writePython3 "record" {
        libraries = [ pkgs.python3Packages.pexpect ];
      } ./record.py
    } "$@"
  '';
}
