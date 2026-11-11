#!/bin/sh
# Build the Mumble HrtfSpatializer harness into .cache/mumble-harness.
# Sources are read from S's Mumble checkout (branch spatial-room) without modifying it.
# Run inside: nix develop .#mumble -c sh bench/mumble/build.sh
set -eu
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
MUMBLE=${MUMBLE:-$HOME/code/mumble}
OUT=$ROOT/.cache/mumble-harness
mkdir -p "$OUT"
[ -f "$ROOT/.cache/CACHEDIR.TAG" ] || printf 'Signature: 8a477f597d28d172789f06886806bc55\n' > "$ROOT/.cache/CACHEDIR.TAG"
SRC=$OUT/src
mkdir -p "$SRC"
git -C "$MUMBLE" show spatial-room:src/mumble/HrtfSpatializer.cpp > "$SRC/HrtfSpatializer.cpp"
git -C "$MUMBLE" show spatial-room:src/mumble/HrtfSpatializer.h > "$SRC/HrtfSpatializer.h"
git -C "$MUMBLE" rev-parse spatial-room > "$OUT/mumble-commit.txt"
FFT=$MUMBLE/3rdparty/fftconvolver
g++ -O2 -std=c++17 -fPIC -I"$SRC" -I"$FFT" $(pkg-config --cflags Qt6Core libmysofa) \
	"$ROOT/bench/mumble/harness.cpp" "$SRC/HrtfSpatializer.cpp" \
	"$FFT/FFTConvolver.cpp" "$FFT/AudioFFT.cpp" "$FFT/Utilities.cpp" \
	$(pkg-config --libs Qt6Core libmysofa) -o "$OUT/mumble-harness"
echo "$OUT/mumble-harness"
