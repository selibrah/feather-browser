#!/bin/sh
# Downloads EasyList and EasyPrivacy into FeatherBrowser's data directory.
#
# The browser reads whatever lists it finds there at startup and compiles them into
# WebKit content rules. Nothing is fetched at runtime, so the binary needs no HTTP client
# and no network access on launch.
#
# ponytail: a shell script instead of an in-process downloader. If you want the lists to
# refresh themselves, that is an HTTP client plus a scheduler plus failure handling — add
# it when you actually want automatic updates, not before.
set -eu

case "$(uname -s)" in
  Darwin) DIR="$HOME/Library/Application Support/FeatherBrowser" ;;
  *)      DIR="${XDG_DATA_HOME:-$HOME/.local/share}/FeatherBrowser" ;;
esac

mkdir -p "$DIR"

fetch() {
  printf 'Fetching %s\n' "$2"
  curl -fsSL "$1" -o "$DIR/$2.part"
  mv "$DIR/$2.part" "$DIR/$2"
}

fetch https://easylist.to/easylist/easylist.txt      easylist.txt
fetch https://easylist.to/easylist/easyprivacy.txt   easyprivacy.txt

printf '\nFilter lists installed in:\n  %s\n\n' "$DIR"
wc -l "$DIR/easylist.txt" "$DIR/easyprivacy.txt"
printf '\nRestart FeatherBrowser to compile them.\n'
