#!/bin/sh
# Fetch the GeneralUser GS soundfont the built-in General MIDI synth
# plays by default, and put it beside the built binary so a source build
# finds it without configuration. Release packages bundle it instead.
#
# GeneralUser GS by S. Christian Collins, redistributed under its own
# licence (bundling and redistribution permitted):
# https://github.com/mrbumpy409/GeneralUser-GS
set -e
cd "$(dirname "$0")/.."
URL="https://raw.githubusercontent.com/mrbumpy409/GeneralUser-GS/main/GeneralUser-GS.sf2"
for dir in target/release target/debug; do
  mkdir -p "$dir"
done
if [ ! -f target/GeneralUser-GS.sf2 ]; then
  echo "fetching GeneralUser-GS.sf2 (~32 MB)..."
  curl -LfsS -o target/GeneralUser-GS.sf2 "$URL"
fi
for dir in target/release target/debug; do
  cp -f target/GeneralUser-GS.sf2 "$dir/GeneralUser-GS.sf2"
done
echo "soundfont in place"
