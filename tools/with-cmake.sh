#!/usr/bin/env bash
# Usage: tools/with-cmake.sh cargo build -p prequal-pingora
# pingora-core's zlib-ng needs cmake on PATH; on Windows without a standalone cmake, borrow VS Build Tools' copy.
set -euo pipefail
if ! command -v cmake >/dev/null; then
  vs_cmake="/c/Program Files (x86)/Microsoft Visual Studio/2022/BuildTools/Common7/IDE/CommonExtensions/Microsoft/CMake/CMake/bin"
  [ -d "$vs_cmake" ] && export PATH="$vs_cmake:$PATH"
fi
exec "$@"
