import AVFoundation
import Foundation

// Checks for the iOS voice-note packaging path, run against the production
// `VoiceNotePackager.swift` and `MP4Canonicalizer.swift` compiled verbatim.
//
// These exist because no Dart test can reach the Swift packager and the
// Xcode test target needs a full Xcode install: `swiftc` from the Command
// Line Tools is enough to run them, so the two claims that would otherwise
// rest on a hand-run probe (the export carries no metadata box the relay
// forbids, and the recorder's AAC priming survives into the edit list) are
// asserted on every run instead.
//
// The fixture is a real CoreAudio recording (`afconvert -f m4af -d aac`),
// the same `AudioFile` writer `AVAudioRecorder` uses, so it carries the
// `udta/meta/ilst/iTunSMPB` priming atom and no `elst`, exactly like a voice
// note recorded on a device.
//
// Usage: voice-note-audio-checks <fixture.m4a>

var failures: [String] = []

func check(_ name: String, _ passed: Bool, _ detail: @autoclosure () -> String = "") {
  if passed {
    print("ok   \(name)")
  } else {
    let suffix = detail()
    failures.append(suffix.isEmpty ? name : "\(name): \(suffix)")
    print("FAIL \(name)\(suffix.isEmpty ? "" : ": \(suffix)")")
  }
}

func packageFixture(_ fixture: URL, container: VoiceNotePackager.Container) -> URL? {
  var packaged: URL?
  let done = DispatchSemaphore(value: 0)
  VoiceNotePackager.package(sourcePath: fixture.path, container: container) { value in
    if let path = value as? String { packaged = URL(fileURLWithPath: path) }
    done.signal()
  }
  guard done.wait(timeout: .now() + VoiceNotePackager.exportTimeout + 10) == .success else {
    return nil
  }
  return packaged
}

func bytes(of url: URL) -> [UInt8] {
  (try? Data(contentsOf: url)).map { [UInt8]($0) } ?? []
}

guard CommandLine.arguments.count > 1 else {
  FileHandle.standardError.write(Data("usage: voice-note-audio-checks <fixture.m4a>\n".utf8))
  exit(2)
}
let fixture = URL(fileURLWithPath: CommandLine.arguments[1])
let fixtureBytes = bytes(of: fixture)

// What the recording itself says, read straight out of its `iTunSMPB` atom
// rather than hard-coded, so the expected edit is the fixture's own priming.
guard let priming = BoxReader.iTunSMPBPriming(fixtureBytes), priming > 0 else {
  FileHandle.standardError.write(Data("fixture has no iTunSMPB priming\n".utf8))
  exit(2)
}
guard let sourceTrack = AVURLAsset(url: fixture).tracks(withMediaType: .audio).first else {
  FileHandle.standardError.write(Data("fixture has no audio track\n".utf8))
  exit(2)
}
let playable = sourceTrack.timeRange.duration
print("fixture priming = \(priming) samples, playable = \(CMTimeGetSeconds(playable))s")

// MARK: - The bare-audio export

guard let audioURL = packageFixture(fixture, container: .m4a) else {
  FileHandle.standardError.write(Data("packaging the bare audio failed\n".utf8))
  exit(1)
}
let audioBytes = bytes(of: audioURL)
let audioBoxes = BoxReader.parse(audioBytes)

check(
  "the bare-audio export is ftyp, moov and mdat only",
  audioBoxes.map(\.type) == ["ftyp", "moov", "mdat"],
  "got \(audioBoxes.map(\.type))"
)

// `udta`, `meta` and `ilst` are on the relay's forbidden list
// (`buzz_media::validate_m4a_file` answers `MetadataForbidden`), and the
// Apple M4A writers add them behind `metadata = []`. `sdtp` is renamed to
// `free` by the canonicalizer.
for forbidden in ["udta", "meta", "ilst", "sdtp"] {
  let found = audioBoxes.flatMap { $0.all(forbidden) }
  check(
    "the bare-audio export carries no \(forbidden) box",
    found.isEmpty,
    "found \(found.count) at \(found.map(\.start))"
  )
}

guard let moov = audioBoxes.first(where: { $0.type == "moov" }),
  let mvhd = moov.child("mvhd"),
  let movieTimescale = BoxReader.timescaleAndDuration(audioBytes, mvhd)?.0,
  let trak = moov.child("trak"),
  let mdhd = trak.descendant(["mdia", "mdhd"]),
  let media = BoxReader.timescaleAndDuration(audioBytes, mdhd),
  let elst = trak.descendant(["edts", "elst"]),
  let entry = BoxReader.singleEditListEntry(audioBytes, elst)
else {
  FileHandle.standardError.write(Data("the bare-audio export has no single-entry edit list\n".utf8))
  exit(1)
}

let expectedMediaTime = CMTimeConvertScale(
  CMTime(value: priming, timescale: CMTimeScale(sourceTrack.naturalTimeScale)),
  timescale: CMTimeScale(media.0),
  method: .roundHalfAwayFromZero
).value
let expectedSegmentDuration = CMTimeConvertScale(
  playable,
  timescale: CMTimeScale(movieTimescale),
  method: .roundHalfAwayFromZero
).value

// Without this the priming samples play as audio: the note is shifted right
// by 48 ms and the same length is cut off the tail.
check(
  "the edit list starts at the recorder's AAC priming",
  entry.1 == expectedMediaTime,
  "media_time \(entry.1), expected \(expectedMediaTime)"
)
check(
  "the edit list presents the playable duration",
  entry.0 == expectedSegmentDuration,
  "segment_duration \(entry.0), expected \(expectedSegmentDuration)"
)
let presentedInMedia = CMTimeConvertScale(
  playable,
  timescale: CMTimeScale(media.0),
  method: .roundHalfAwayFromZero
).value
check(
  "the edit fits inside the written media",
  entry.1 > 0 && entry.1 + presentedInMedia <= media.1,
  "media_time \(entry.1) plus \(presentedInMedia) exceeds media duration \(media.1)"
)

// MARK: - The guards around the restore

let alreadyEdited = try MP4Canonicalizer.restoreAudioEditListPriming(
  at: audioURL,
  mediaTime: CMTime(value: 1, timescale: 44100),
  segmentDuration: playable
)
check(
  "an edit list that already carries a media_time is left alone",
  alreadyEdited == false && bytes(of: audioURL) == audioBytes
)

// The same file with the edit flattened again is the input the writer
// actually produces; an edit longer than the media it addresses must be
// refused rather than shifting a track whose samples are already trimmed.
let flattened = audioURL.deletingLastPathComponent()
  .appendingPathComponent("flattened-\(UUID().uuidString).m4a")
var flattenedBytes = audioBytes
let mediaTimeOffset = elst.payloadStart + 8 + (audioBytes[elst.payloadStart] == 1 ? 8 : 4)
let mediaTimeWidth = audioBytes[elst.payloadStart] == 1 ? 8 : 4
for index in 0..<mediaTimeWidth { flattenedBytes[mediaTimeOffset + index] = 0 }
try Data(flattenedBytes).write(to: flattened)

let tooLong = try MP4Canonicalizer.restoreAudioEditListPriming(
  at: flattened,
  mediaTime: CMTime(value: priming, timescale: CMTimeScale(media.0)),
  segmentDuration: CMTime(value: media.1, timescale: CMTimeScale(media.0))
)
check(
  "an edit that would run past the written media is refused",
  tooLong == false && bytes(of: flattened) == flattenedBytes
)

let restored = try MP4Canonicalizer.restoreAudioEditListPriming(
  at: flattened,
  mediaTime: CMTime(value: priming, timescale: CMTimeScale(media.0)),
  segmentDuration: playable
)
check(
  "a flattened edit list is restored in place",
  restored && bytes(of: flattened).count == flattenedBytes.count
)

// MARK: - The envelope export

guard let envelopeURL = packageFixture(fixture, container: .mp4Envelope) else {
  FileHandle.standardError.write(Data("packaging the envelope failed\n".utf8))
  exit(1)
}
let envelopeBytes = bytes(of: envelopeURL)
guard let envelopeMoov = BoxReader.parse(envelopeBytes).first(where: { $0.type == "moov" }) else {
  FileHandle.standardError.write(Data("the envelope has no moov\n".utf8))
  exit(1)
}

var soundEntry: (Int64, Int64)?
var otherEntries: [(Int64, Int64)] = []
for envelopeTrak in envelopeMoov.children.filter({ $0.type == "trak" }) {
  guard let hdlr = envelopeTrak.descendant(["mdia", "hdlr"]),
    let elst = envelopeTrak.descendant(["edts", "elst"]),
    let entry = BoxReader.singleEditListEntry(envelopeBytes, elst)
  else { continue }
  if BoxReader.handlerType(envelopeBytes, hdlr) == "soun" {
    soundEntry = entry
  } else {
    otherEntries.append(entry)
  }
}

check(
  "the envelope's audio track carries the same priming edit",
  soundEntry?.1 == expectedMediaTime,
  "media_time \(String(describing: soundEntry?.1)), expected \(expectedMediaTime)"
)
check(
  "the envelope's video track is not given an audio edit",
  !otherEntries.isEmpty && otherEntries.allSatisfy { $0.1 == 0 },
  "entries \(otherEntries)"
)

for temporary in [audioURL, flattened, envelopeURL] {
  try? FileManager.default.removeItem(at: temporary)
}

if failures.isEmpty {
  print("\nall voice-note audio checks passed")
  exit(0)
}
print("\n\(failures.count) check(s) failed:")
for failure in failures { print("  - \(failure)") }
exit(1)
