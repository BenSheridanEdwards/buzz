# Voice notes as a first-class feature, mobile and desktop

Design canvas (chosen direction, all states): https://claude.ai/code/artifact/36adfd3b-88d6-47e5-857c-4b8dba713436
Design source: `design/voice-notes/` on `feature/audio-attachments`. Technical scope: `docs/AUDIO-ATTACHMENTS-SCOPE.md`.

## Why

Dictation in and voice replies out are non-negotiable for how Chief works with agents. Today Buzz:

- rejects every `audio/*` upload at the relay (`crates/buzz-media/src/validation.rs`), so an agent's MP3 reply can only arrive as a link or as an MP4 video envelope;
- ships no mobile build with voice notes (newest tag `mobile-v0.16.0-rc.2` predates the 3 Sep merge of PR #7121);
- has no lock, slide-to-cancel, review, speed or transcript on the desktop recorder it does ship.

The bar is Telegram: record with one thumb, lock it, review it, and play what comes back inline with a waveform and a transcript.

## The design (direction A, chosen 2026-09-06)

Mobile composer, one control that changes with context:

1. **Idle.** Plus on the left, mic on the right, 44px target.
2. **Typing.** The mic slot becomes Send as soon as there is text; clearing the text brings the mic back. Same slot, no layout shift.
3. **Hold to record.** Pill becomes a recorder: red dot, running timer, live waveform, "Slide to cancel". Slide left past the threshold discards. Slide up into the lock chip goes hands free. Release sends.
4. **Locked.** Trash, pause/resume, full-width Send. Cap 5:00 (matches the existing recorder).
5. **Review** (setting, default off). Listen back, re-record, or send.

Received voice note (mobile and desktop), extending Buzz's attachment card:

- play/pause, waveform that fills as it plays, current/total time, sender as the title;
- speed pill cycling 1×, 1.5×, 2×;
- transcript row with a header toggle: open by default in DMs, folded in channels, last choice remembered per device;
- desktop keeps download.

Desktop composer keeps the existing recorder row and adds: hold mic or hold Space to record, hold L or click the chip to lock, pause and delete when locked.

## Acceptance

- [ ] Chief records a voice note on the phone with hold, slide-to-cancel, slide-to-lock, locked pause/delete/send; the agent receives speech and Hermes transcribes it.
- [ ] Same on desktop with keyboard equivalents (Space, L, Esc to cancel).
- [ ] An agent's MP3 reply renders as the card above on both clients, plays inline, scrubs on the waveform, honours the speed pill.
- [ ] The uploaded file is real audio (`audio/mpeg` or `audio/mp4`), not a video container.
- [ ] Transcript toggle folds and remembers its state.
- [ ] Every control has one accessible label and a keyboard path (repo rules 7 and 8 in AGENTS.md).
- [ ] Works against a relay without the feature: clients read NIP-11 `supported_extensions` and fall back to the current behaviour.

## Plan

| Milestone | Scope | Outcome |
|---|---|---|
| M1 | Relay accepts `audio/*` behind NIP-11 `buzz-audio`; metadata stripped; CLI allow-list and `filename` imeta | Agents send real MP3; desktop plays inline |
| M2 | Hermes Buzz plugin: direct Blossom upload of TTS output; inbound voice notes and `audio/*` classified as audio for STT | Chief's voice reaches agents as speech |
| M3 | Mobile from `main` plus the recorder states above (lock, cancel, review) and the card | Phone matches the design |
| M4 | Desktop recorder: lock, pause, keyboard path; card: speed, transcript toggle | Desktop matches the design |
| M5 | Attach arbitrary audio files from either client; upstream PR behind the flag | Done, upstreamable |

Details per component, effort and risks: `docs/AUDIO-ATTACHMENTS-SCOPE.md`.

## Open decisions

1. Start M1 and M2 now (about two days, no phone change)?
2. Android toolchain on the Studio for M3 (SDK and NDK, about 3 GB, sideloaded APK), or wait for upstream mobile 0.17?
