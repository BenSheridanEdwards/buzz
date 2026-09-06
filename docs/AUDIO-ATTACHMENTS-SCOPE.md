# Real audio in Buzz: scope for a Studio fork

Branch: `feature/audio-attachments` on `~/Chief/Projects/buzz` (cloned from block/buzz `main` at 3c7f288c6, 2026-09-05).
Written 2026-09-06 after a day of probing the relay, CLI, desktop and mobile code.

## 1. The goal, stated plainly

Chief speaks, agents hear. Agents speak, Chief hears. In Buzz, on the phone and on the desktop, with audio that plays inline and is a real audio file.

Non-negotiables:
1. **Dictation**: a voice note recorded in Buzz reaches the agent as speech and is transcribed (Hermes already does STT with xAI).
2. **Voice replies**: an agent's spoken reply arrives in Buzz as an audio attachment with an inline player, on mobile and desktop.
3. **Real audio files**: the attachment is audio (MP3, M4A or Ogg), not a video container with a fake track.

## 2. Where upstream Buzz is today

| Piece | State on `main` (2026-09-05) | State in shipped builds |
|---|---|---|
| Relay media upload | Rejects every `audio/*` type. `crates/buzz-media/src/validation.rs` `validate_file_content` has an explicit block with the comment "audio is rejected until Buzz has an explicit sanitizer and location-metadata validator for its container". | Same. Our relay runs the 2026-09-05 image. |
| Voice notes, desktop | Shipped in desktop 0.5.19 (1 Sep). Records WAV, packages as H.264/AAC MP4, uploads as `video/mp4`, marks it with imeta `filename voice-note-<id>.mp4`; renderer shows a voice-note player. | In 0.5.22 and 0.5.23. |
| Voice notes, mobile | Merged 3 Sep (PR #7121). Records M4A, same MP4 envelope trick. | **Not in any release.** Newest tag `mobile-v0.16.0-rc.2` is from 1 Sep and predates the merge. The store build has no microphone. |
| Plain audio rendering, desktop | Exists: `desktop/src/features/messages/lib/audioAttachment.ts` treats any `audio/*` imeta as an audio attachment; `AudioMessageAttachment.tsx` renders `<audio>` with an inline player. | Yes, since 0.5.19. |
| Plain audio rendering, mobile | Exists on `main`: `message_media.dart` classifies `audio/*` as `MessageMediaKind.audio`; `message_content.dart:328` renders it. | Not released. |
| CLI `buzz upload file` | Local allow-list `ALLOWED_MIMES` in `crates/buzz-cli/src/client.rs:1178` rejects audio before upload. `build_imeta_tag` (line 40) never writes a `filename` field, so the CLI cannot mark a voice note. | Same in 0.5.22 and 0.5.23. |

Conclusion: the clients are mostly ready for audio; the relay and the CLI are what block it, and the mobile app needs a build from `main` regardless.

## 3. The change, by component

### 3.1 Relay (`crates/buzz-media`, `crates/buzz-relay`)

Minimal, honouring upstream's stated reason for the block (metadata leakage):

1. `validation.rs`
   - New `validate_audio_content(bytes, config) -> (mime, ext)`: accept `audio/mpeg`, `audio/mp4`, `audio/ogg`, `audio/aac`, `audio/flac`, `audio/wav` by magic bytes; enforce a new `max_audio_bytes` cap.
   - `serve_inline`: return true for `audio/*` so clients get `Content-Disposition: inline`.
   - Leave `validate_file_content`'s audio rejection in place; audio goes through its own path.
2. `upload.rs`
   - New `process_audio_upload`: like `process_video_upload`, run ffmpeg to re-encode into a canonical container with `-map_metadata -1` (strips ID3, GPS, cover art), probe duration, store, write a sidecar with `duration`. This is the "sanitizer" upstream asked for.
   - Reuse `run_ffmpeg` and the temp-file pattern already in `process_video_upload`.
3. `config.rs`: `max_audio_bytes` (default 25 MB) and env `BUZZ_MAX_AUDIO_BYTES`.
4. `crates/buzz-relay/src/api/media.rs`
   - Upload dispatch (around lines 360 to 420): route sniffed `audio/*` to `process_audio_upload` on both `/upload` and `/media/upload`.
   - GET content-type map (around line 660): map `.mp3`, `.m4a`, `.ogg`, `.flac`, `.wav` to their audio types instead of the generic download type.
5. Tests: unit tests beside the video ones; one conformance test in `crates/buzz-conformance` that uploads an MP3 and reads it back inline.

Size: about 300 lines including tests. One to two days for a careful version; half a day for a "accept, strip, serve" version.

### 3.2 CLI (`crates/buzz-cli`)

1. `client.rs:1178` `ALLOWED_MIMES`: add the audio types; size cap uses `MAX_AUDIO_BYTES`.
2. `build_imeta_tag`: add `filename <name>` from the local path so voice notes and named files survive the CLI path. Clients already read `filename`.
3. `messages send --file` needs nothing else.

Size: under 50 lines. One to two hours.

### 3.3 Desktop (`desktop/`)

Receiving needs no change. Sending does:
1. `desktop/src-tauri/src/commands/media.rs` `detect_and_validate_mime`: allow `audio/*` so a user can attach an audio file.
2. `media_voice_note.rs` and `media_transcode.rs`: once the relay accepts audio, the recorder can upload real AAC (`audio/mp4`) instead of the MP4 envelope. Keep the envelope path as a fallback for relays that do not accept audio, keyed on the relay's NIP-11 (see 3.6).
3. `imetaMediaMarkdown.ts`: audio lines already format as `[name](url)`; no change.

Size: about 100 lines. Half a day. Optional for the first milestone, because the desktop already plays what agents send.

### 3.4 Mobile (`mobile/`)

1. Build from `main` to get voice notes at all (PR #7121).
2. `mobile/lib/shared/relay/media_upload.dart`: `_allowedAudioMimeTypes` is the voice-note set; extend the generic upload path (line 585) to accept audio files from the picker.
3. `voice_note_recording.dart` and the two native packagers: upload the recorded M4A as `audio/mp4` when the relay accepts audio; keep the MP4 envelope as fallback.
4. `message_content.dart:328`: verify the audio kind renders the same player as voice notes for `audio/mpeg` and `audio/mp4`; adjust the kind detection in `message_media.dart:123-141` if it keys on filename.

Size: about 150 lines plus the build pipeline. One day of code. Toolchain is the real cost, see section 5.

### 3.5 Hermes side (`~/.hermes/hermes-agent/plugins/platforms/buzz/adapter.py`)

1. Outbound: `send_voice` uploads the MP3 straight to the relay's Blossom `/upload` with a kind-24242 auth event (the plugin already has the signing primitives in `nostr_auth.py`), then publishes the kind-9 message with imeta `m audio/mpeg`, `filename <agent>-<ts>.mp3`, `duration`. This bypasses the CLI gate entirely, so 3.2 is not on the critical path for agents.
2. Inbound: classify a voice note (`m video/mp4` with `filename voice-note-*.mp4`) and any `audio/*` attachment as `MessageType.AUDIO` so Hermes runs STT. Today a Buzz voice note is classified as video and skipped.
3. Strip metadata from TTS output before upload (`ffmpeg -map_metadata -1`) so the relay's sanitizer has nothing to do.

Size: about 150 lines. Half a day. Already partially done: the current patch builds the MP4 envelope and publishes the event directly; it becomes simpler, not harder.

### 3.6 Compatibility switch

Advertise audio support in the relay's NIP-11 (`supported_extensions: ["buzz-audio"]`). Clients and the Hermes plugin read it and choose real audio when present, MP4 envelope otherwise. That keeps the fork usable against upstream relays and keeps the patch upstreamable.

## 4. Milestones

| # | Outcome | Needs | Effort |
|---|---|---|---|
| M1 | Agents send real MP3 voice replies; desktop plays them inline | 3.1 minimal, 3.5, relay rebuild | 1.5 days |
| M2 | Chief's voice notes reach agents as speech, from desktop | 3.5 inbound fix | included in M1 |
| M3 | Phone: voice notes both ways with inline player | 3.4 build from main + audio accept, APK sideload | 1.5 days plus toolchain |
| M4 | Attach arbitrary audio files from either client | 3.3, 3.4 picker | 1 day |
| M5 | Upstream PR: audio pipeline behind NIP-11 flag | tests, docs | 1 day |

M1 and M2 are the unblock. M3 is what makes the phone match.

## 5. Build and deploy on the Studio

- **Relay**: `docker build -t buzz-relay:audio .` from the fork root using the upstream `Dockerfile` (Rust 1.95, cargo-chef). Colima, 12 cores: expect 30 to 60 minutes cold, minutes warm. Then `BUZZ_IMAGE=buzz-relay:audio` in `~/Chief/Projects/relay/.env` and `docker compose up -d`. The pair sidecar uses the same image.
- **CLI**: `cargo build -p buzz-cli --release` via the repo's hermit toolchain (`bin/cargo`). Point the Hermes plugin `cli_path` at the built binary, or skip: 3.5 does not need it.
- **Mobile (Android, Pixel 10 Pro XL)**: the repo bootstraps Flutter 3.41.7 through hermit (`bin/.flutter-3.41.7.pkg`). Still missing on the Studio: Android SDK and NDK (`sdkmanager`, about 3 GB), Java 17 (system Java present, version unverified), an upload keystore for release builds. Build: `bin/just mobile-install` for a debug install over adb, or `flutter build apk --release` and sideload. adb reaches the phone over USB or Wireless Debugging on the tailnet; pairing is a one-time phone step.
- **Desktop**: not needed for M1 to M3; upstream 0.5.23 already plays audio. Build from the fork only for M4 (Tauri: `pnpm` plus `cargo`, both via hermit).

## 6. Risks and honest notes

- The relay block exists for a reason: audio containers carry metadata (ID3, GPS in M4A). The re-encode with `-map_metadata -1` is the mitigation, and it is the same tool upstream already uses for video.
- ffmpeg must be in the relay image. The upstream runtime image is `debian-slim`; check whether it ships ffmpeg for the video pipeline (it must, since `process_video_upload` calls it) before assuming.
- Mobile from `main` means living on tip until upstream ships 0.17. Sideloaded builds do not auto-update; reinstall on each rebase.
- Keep every change behind the NIP-11 flag so the fork stays a thin patch and can go upstream as a PR. Upstream's own comment signals they want this.
- Two Skys of the same name in one workspace confuse humans and agents; the same applies to two relays. There will be one relay, the patched one.

## 7. Decision needed from Chief

1. Go on M1 and M2 now (relay patch, rebuild, plugin), roughly two days of work, no change to the phone.
2. Whether to invest in the Android toolchain for M3, or wait for upstream mobile 0.17 to ship voice notes and only patch the relay side.
