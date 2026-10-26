#!/usr/bin/env bash
set -euo pipefail

source_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
data_home=${XDG_DATA_HOME:-"$HOME/.local/share"}
cache_home=${XDG_CACHE_HOME:-"$HOME/.cache"}
source_key=$(printf '%s' "$source_dir" | sha256sum)
source_key=${source_key%% *}
build_dir=${VOICE_INPUT_BUILD_DIR:-"$cache_home/voice-input/fcitx5/$source_key"}

cmake -S "$source_dir" -B "$build_dir" \
    -DCMAKE_BUILD_TYPE=Release \
    -DVOICE_INPUT_DATA_HOME="$data_home"
cmake --build "$build_dir" --parallel
cmake --install "$build_dir"

printf 'Installed the Fcitx5 addon under %s\n' "$data_home"
printf 'Restart Fcitx5 with: fcitx5 -d -r\n'
