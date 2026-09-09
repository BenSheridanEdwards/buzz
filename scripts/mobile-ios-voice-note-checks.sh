#!/usr/bin/env bash
#
# Runs the iOS voice-note packaging checks against the production Swift.
#
# No Xcode install is required: a stub `Flutter` module is compiled first so
# `mobile/ios/Runner/VoiceNotePackager.swift` compiles verbatim, with its
# `import Flutter` untouched. AVFoundation does the rest, so this only runs on
# macOS. See mobile/ios/tools/voice-note-audio-checks/main.swift.
set -euo pipefail

if [[ "$(uname -s)" != "Darwin" ]]; then
    echo "skipping: the iOS voice-note checks need macOS AVFoundation" >&2
    exit 0
fi

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
tools="${repo_root}/mobile/ios/tools/voice-note-audio-checks"
runner="${repo_root}/mobile/ios/Runner"
build="$(mktemp -d)"
trap 'rm -rf "${build}"' EXIT

swiftc -emit-module -emit-library -static \
    -module-name Flutter \
    -emit-module-path "${build}/Flutter.swiftmodule" \
    -o "${build}/libFlutterStub.a" \
    "${tools}/FlutterStub.swift"

swiftc -O -I "${build}" -L "${build}" -lFlutterStub \
    -o "${build}/voice-note-audio-checks" \
    "${tools}/main.swift" \
    "${tools}/BoxReader.swift" \
    "${runner}/VoiceNotePackager.swift" \
    "${runner}/MP4Canonicalizer.swift"

"${build}/voice-note-audio-checks" "${tools}/fixtures/recorder-coreaudio.m4a"
