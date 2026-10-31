#!/usr/bin/env bash
set -euo pipefail

source_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
data_home=${XDG_DATA_HOME:-"$HOME/.local/share"}
cache_home=${XDG_CACHE_HOME:-"$HOME/.cache"}
if ! command -v sha256sum >/dev/null 2>&1; then
    printf 'sha256sum is required; add coreutils to the build environment\n' >&2
    exit 1
fi
source_key=$(printf '%s' "$source_dir" | sha256sum)
source_key=${source_key%% *}
build_dir=${LIMEN_BUILD_DIR:-"$cache_home/limen/fcitx5/$source_key"}

cmake -S "$source_dir" -B "$build_dir" \
    -DCMAKE_BUILD_TYPE=Release \
    -DLIMEN_DATA_HOME="$data_home"
cmake --build "$build_dir" --parallel
cmake --install "$build_dir"

printf 'Installed the Fcitx5 addon under %s\n' "$data_home"
printf 'Restart Fcitx5 with: fcitx5 -d -r\n'
