#!/bin/sh
# Install gitta: picks the build for this OS and CPU, verifies its SHA-256 checksum, installs to ~/.local/bin
# (or $GITTA_BIN_DIR). The release workflow fills in RELEASES and VERSION.
# Downloads go through https://gitta.io/dl, which counts them (via=install, no personal data), and fall back to RELEASES.
# GITTA_RELEASES skips gitta.io; GITTA_DL sets another download base.
# The script changes no file outside the bin dir, except your shell startup file: when the bin dir is not on PATH, it
# adds one line there, once, without asking, and says which file it changed.
set -eu
DL=""; [ -z "${GITTA_RELEASES:-}" ] && DL="${GITTA_DL:-https://gitta.io/dl}"
RELEASES="${GITTA_RELEASES:-@RELEASES@}"
VERSION="${GITTA_VERSION:-@VERSION@}"
BIN_DIR="${GITTA_BIN_DIR:-$HOME/.local/bin}"
DOCS="https://gitta.io/docs"

# Colour and bold only at a terminal, and never when NO_COLOR is set.
if [ -t 1 ] && [ -z "${NO_COLOR:-}" ]; then B=$(printf '\033[1m'); D=$(printf '\033[2m'); G=$(printf '\033[32m'); Y=$(printf '\033[33m'); R=$(printf '\033[0m')
else B=""; D=""; G=""; Y=""; R=""; fi
say() { printf '%s\n' "$*"; }
warn() { printf '%s\n' "${Y}Note:${R} $*"; }
# Show paths under HOME as ~/..., so the lines stay short.
tilde() { case "$1" in "$HOME"/*) printf '~/%s' "${1#"$HOME"/}" ;; *) printf '%s' "$1" ;; esac; }
die() { printf '%s\n' "gitta: $*" >&2; exit 1; }

case "$VERSION$RELEASES" in *@*) die "this is the unreleased install script; set GITTA_RELEASES and GITTA_VERSION, or use the install.sh of a release" ;; esac
bin=$(tilde "$BIN_DIR")
os=$(uname -s); arch=$(uname -m)
case "$os-$arch" in
  Darwin-arm64) target=aarch64-apple-darwin; what="macOS, Apple silicon" ;;
  Darwin-x86_64) target=x86_64-apple-darwin; what="macOS, Intel" ;;
  Linux-x86_64|Linux-amd64) target=x86_64-unknown-linux-musl; what="Linux x86_64" ;;
  Linux-aarch64|Linux-arm64) target=aarch64-unknown-linux-musl; what="Linux arm64" ;;
  MINGW*|MSYS*|CYGWIN*) die "on Windows, install gitta from PowerShell:  irm https://gitta.io/install.ps1 | iex" ;;
  *) die "there is no build for $os $arch yet. You can build it from source: $DOCS/install.html" ;;
esac
command -v curl >/dev/null 2>&1 || case "$RELEASES" in file://*) ;; *) die "curl is missing. Install curl, then run this script again." ;; esac

old=""; [ -x "$BIN_DIR/gitta" ] && old=$("$BIN_DIR/gitta" --version 2>/dev/null | awk '{print $NF; exit}') || true
if [ -z "$old" ]; then say "Installing ${B}gitta $VERSION${R} for $what into $bin"
elif [ "$old" = "$VERSION" ]; then say "Reinstalling ${B}gitta $VERSION${R} for $what into $bin"
else say "Upgrading gitta from $old to ${B}$VERSION${R} for $what in $bin"; fi

name="gitta-$VERSION-$target"
tmp="$(mktemp -d)"; trap 'rm -rf "$tmp"' EXIT
get() { case "$1" in file://*) cp "${1#file://}" "$2" ;; *) curl -fsSL "$1" -o "$2" ;; esac; }
# fetch FILE: from gitta.io first, then straight from the release.
fetch() {
  if [ -n "$DL" ] && get "$DL/v$VERSION/$1?via=install" "$tmp/$1" 2>/dev/null; then return 0; fi
  get "$RELEASES/$1" "$tmp/$1" 2>/dev/null || die "cannot download $RELEASES/$1. Check the network connection and try again."
}
fetch "$name.tar.gz"
fetch "$name.tar.gz.sha256"
want="$(cut -d' ' -f1 < "$tmp/$name.tar.gz.sha256")"
have="$( (shasum -a 256 "$tmp/$name.tar.gz" 2>/dev/null || sha256sum "$tmp/$name.tar.gz") | cut -d' ' -f1)"
[ "$want" = "$have" ] || die "the checksum does not match, so nothing was installed. Try again, and report it if it happens twice."
tar -xzf "$tmp/$name.tar.gz" -C "$tmp"
mkdir -p "$BIN_DIR"
# Write next to the old file, then rename over it, so a running gitta is never changed in place.
for f in "$tmp/$name"/*; do b=${f##*/}; install -m 0755 "$f" "$BIN_DIR/.$b.new" && mv -f "$BIN_DIR/.$b.new" "$BIN_DIR/$b"; done
say "${G}Done.${R} Checksum verified, $(cd "$tmp/$name" && echo *) installed."

# PATH: add the line to the user's shell startup file once, and say where.
case ":$PATH:" in *":$BIN_DIR:"*) ;; *)
  pb=$BIN_DIR; case "$BIN_DIR" in "$HOME"/*) pb="\$HOME/${BIN_DIR#"$HOME"/}" ;; esac
  case "${SHELL:-}" in
    */zsh) rc="$HOME/.zshrc"; line="export PATH=\"$pb:\$PATH\"" ;;
    */fish) rc="$HOME/.config/fish/config.fish"; line="fish_add_path $pb" ;;
    */bash) rc="$HOME/.bashrc"; [ "$os" = Darwin ] && rc="$HOME/.bash_profile"; line="export PATH=\"$pb:\$PATH\"" ;;
    *) rc="$HOME/.profile"; line="export PATH=\"$pb:\$PATH\"" ;;
  esac
  mkdir -p "$(dirname "$rc")"
  if ! grep -qsF "$line" "$rc"; then printf '\n# added by the gitta installer\n%s\n' "$line" >> "$rc"; fi
  say ""; say "Added $bin to your PATH in $(tilde "$rc"). Open a new terminal, or run:  ${B}$line${R}" ;;
esac

# Prerequisites and agent tools.
say ""
if ! command -v git >/dev/null 2>&1; then warn "git is not installed, and gitta needs it. Install git 2.28 or later, then continue below."
else
  gv=$(git --version | awk '{print $3}'); gmaj=${gv%%.*}; gmin=${gv#*.}; gmin=${gmin%%.*}
  if [ "$gmaj" -lt 2 ] 2>/dev/null || { [ "$gmaj" -eq 2 ] && [ "$gmin" -lt 28 ]; } 2>/dev/null; then
    warn "git $gv is older than 2.28. gitta still checks every merge with a fallback hook, but git 2.28 or later stops a failed fast-forward before it lands. Update git when you can."
  else say "Found git $gv."; fi
fi
for a in "claude:.claude:Claude Code" "cursor:.cursor:Cursor" "codex:.codex:Codex"; do
  cmd=${a%%:*}; rest=${a#*:}; dir=${rest%%:*}; label=${rest#*:}
  if command -v "$cmd" >/dev/null 2>&1 || [ -d "$HOME/$dir" ]; then say "Found $label. \`gitta init\` will connect it."; fi
done

say ""
say "${B}Next steps${R}"
say "  1. Go to your repository:      cd path/to/your/repo"
say "  2. Set it up with your tests:  ${B}gitta init --gate \"<your test command>\"${R}"
say "     ${D}for example${R} --gate \"pytest\", --gate \"npm test\", --gate \"cargo test\" or --gate \"go test ./...\""
say "  3. See what gitta does:        gitta watch"
say "  Quick start: $DOCS/quickstart.html"
