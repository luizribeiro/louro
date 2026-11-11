# Terminal demos, recorded inside the VM tests in Luiz's terminal look: the
# recorder is copied from snowglobe's nix/demos, and the render settings
# follow Luiz's terminal-look notes (agg, then ffmpeg for social clips).
{ pkgs }:
let
  inherit (pkgs) lib;
  lookFile = ./terminal-look.json;
  look = lib.importJSON lookFile;
  aggTheme = lib.concatMapStringsSep "," (lib.removePrefix "#") (
    [
      look.bg
      look.fg
    ]
    ++ look.palette
  );
  fontDirs = map (font: "${font}/share/fonts") [
    pkgs.nerd-fonts.hack
    pkgs.dejavu_fonts
    pkgs.noto-fonts-monochrome-emoji
  ];
  pad16by9 = "pad=w='max(iw,ceil(ih*16/9/2)*2)':h='max(ih,ceil(iw*9/16/2)*2)':x=(ow-iw)/2:y=(oh-ih)/2:color=${look.bg}";
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

  render = pkgs.writeShellApplication {
    name = "render-demo";
    runtimeInputs = [
      pkgs.asciinema-agg
      pkgs.ffmpeg
    ];
    text = ''
      if [ $# -ne 2 ]; then
        echo "usage: render-demo CAST OUT_DIR  (writes OUT_DIR/NAME.svg, NAME.gif and NAME.mp4)" >&2
        exit 2
      fi
      name=$(basename "$1" .cast)
      mkdir -p "$2"
      TERMINAL_LOOK=${lookFile} SVG_TERM=${pkgs.svg-term}/bin/svg-term \
        ${pkgs.writers.writePython3 "cast-to-svg" { } ./cast_to_svg.py} "$1" "$2/$name.svg"
      agg --quiet \
        ${lib.concatMapStringsSep " " (dir: "--font-dir ${dir}") fontDirs} \
        --font-family "Hack Nerd Font Mono,DejaVu Sans Mono,Noto Emoji" \
        --theme ${aggTheme} \
        --font-size 24 \
        --idle-time-limit 1 \
        --last-frame-duration 4 \
        "$1" "$2/$name.gif"
      # fps=30: the GIF's frames last until the screen changes, and social
      # sites want a constant frame rate.
      ffmpeg -loglevel error -y -i "$2/$name.gif" \
        -movflags faststart -pix_fmt yuv420p \
        -vf "fps=30,scale=trunc(iw/2)*2:trunc(ih/2)*2,${pad16by9}" \
        "$2/$name.mp4"
    '';
  };
}
