import CoreMedia
import Foundation

enum MP4Canonicalizer {
  /// Restores the encoder priming an AAC export leaves out of its edit list.
  ///
  /// CoreAudio's file writer, the one behind `AVAudioRecorder` and so the one
  /// that writes every recorded voice note on iOS, records the AAC priming
  /// only in `udta/meta/ilst/iTunSMPB` and writes no `elst`. AVFoundation
  /// reads that atom and reports the trim on `AVAssetTrack.segments`, but a
  /// passthrough `AVAssetWriter` writes a single `elst` entry with
  /// `media_time = 0`. The priming samples then play as audio: the note is
  /// shifted right by the priming (2112 samples, 48 ms, for `AVAudioRecorder`
  /// output) and the same length is cut off the tail.
  ///
  /// [mediaTime] is the source track's mapped start (`segments.first`'s
  /// `timeMapping.source.start`) and [segmentDuration] its playable duration
  /// (`timeRange.duration`). Both are converted into the file's own `mdhd`
  /// and `mvhd` timescales, and the entry is rewritten in place: it keeps its
  /// size, so no chunk offset moves.
  ///
  /// Fails closed, and returns whether anything was rewritten. The entry is
  /// only touched when the file is exactly the shape this defect produces:
  /// one sound track, one `elst` entry, `media_time` still 0, a non-zero
  /// source mapping, and a media duration long enough to contain
  /// `mediaTime + segmentDuration`. That last check is what keeps a
  /// re-encoded track, whose samples are already trimmed, from being shifted
  /// by an edit it does not need. An export that already carries the right
  /// edit (ffmpeg-authored sources write `media_time` explicitly and the
  /// writer preserves it) is left alone.
  @discardableResult
  static func restoreAudioEditListPriming(
    at url: URL,
    mediaTime: CMTime,
    segmentDuration: CMTime
  ) throws -> Bool {
    guard mediaTime.isNumeric, segmentDuration.isNumeric,
      CMTimeCompare(mediaTime, .zero) > 0, CMTimeCompare(segmentDuration, .zero) > 0
    else { return false }

    var data = try Data(contentsOf: url)
    guard let moov = try boxes(in: data, start: 0, end: data.count).first(ofType: "moov") else {
      return false
    }
    let moovChildren = try boxes(in: data, start: moov.contentStart, end: moov.end)
    guard let mvhd = moovChildren.first(ofType: "mvhd"),
      let movieTimescale = try timescale(in: data, of: mvhd),
      let movieDuration = convert(segmentDuration, to: movieTimescale),
      movieDuration > 0
    else { return false }

    var patched = false
    for trak in moovChildren.all(ofType: "trak") {
      let trakChildren = try boxes(in: data, start: trak.contentStart, end: trak.end)
      guard let mdia = trakChildren.first(ofType: "mdia") else { continue }
      let mdiaChildren = try boxes(in: data, start: mdia.contentStart, end: mdia.end)
      guard let hdlr = mdiaChildren.first(ofType: "hdlr"),
        try handlerType(in: data, of: hdlr) == Array("soun".utf8),
        let mdhd = mdiaChildren.first(ofType: "mdhd"),
        let mediaTimescale = try timescale(in: data, of: mdhd),
        let mediaDuration = try duration(in: data, of: mdhd),
        let editStart = convert(mediaTime, to: mediaTimescale),
        let editDuration = convert(segmentDuration, to: mediaTimescale),
        editStart > 0,
        editStart + editDuration <= mediaDuration,
        let edts = trakChildren.first(ofType: "edts"),
        let elst = try boxes(in: data, start: edts.contentStart, end: edts.end).first(ofType: "elst")
      else { continue }

      guard elst.end - elst.contentStart >= 8 else { throw invalidMp4BoxError() }
      let version = data[elst.contentStart]
      let entryCount = readBigEndianUInt32(data, at: elst.contentStart + 4)
      let entrySize = version == 1 ? 20 : 12
      guard entryCount == 1, elst.contentStart + 8 + entrySize <= elst.end else { continue }

      let entry = elst.contentStart + 8
      if version == 1 {
        guard readBigEndianUInt64(data, at: entry + 8) == 0 else { continue }
        writeBigEndianUInt64(&data, at: entry, UInt64(movieDuration))
        writeBigEndianUInt64(&data, at: entry + 8, UInt64(editStart))
      } else {
        guard readBigEndianUInt32(data, at: entry + 4) == 0 else { continue }
        guard movieDuration <= Int64(UInt32.max), editStart <= Int64(Int32.max) else { continue }
        writeBigEndianUInt32(&data, at: entry, UInt32(movieDuration))
        writeBigEndianUInt32(&data, at: entry + 4, UInt32(editStart))
      }
      patched = true
    }

    if patched { try data.write(to: url, options: .atomic) }
    return patched
  }

  static func neutralizeSampleDependencyBoxes(at url: URL) throws {
    var data = try Data(contentsOf: url)
    try neutralizeSampleDependencyBoxes(in: &data, start: 0, end: data.count)
    try data.write(to: url, options: .atomic)
  }

  private static func neutralizeSampleDependencyBoxes(
    in data: inout Data,
    start: Int,
    end: Int
  ) throws {
    let containers: Set<[UInt8]> = [
      Array("moov".utf8), Array("trak".utf8), Array("mdia".utf8),
      Array("minf".utf8), Array("stbl".utf8), Array("edts".utf8),
      Array("dinf".utf8), Array("sinf".utf8), Array("schi".utf8),
    ]
    let sampleDependencyType = Array("sdtp".utf8)
    let freeType = Array("free".utf8)
    var offset = start

    while offset < end {
      guard end - offset >= 8 else { throw invalidMp4BoxError() }
      let compactSize = Int(readBigEndianUInt32(data, at: offset))
      var headerSize = 8
      let boxSize: Int
      if compactSize == 1 {
        guard end - offset >= 16 else { throw invalidMp4BoxError() }
        let extendedSize = readBigEndianUInt64(data, at: offset + 8)
        guard extendedSize <= UInt64(Int.max) else { throw invalidMp4BoxError() }
        boxSize = Int(extendedSize)
        headerSize = 16
      } else if compactSize == 0 {
        boxSize = end - offset
      } else {
        boxSize = compactSize
      }

      guard boxSize >= headerSize, offset + boxSize <= end else {
        throw invalidMp4BoxError()
      }
      let type = Array(data[(offset + 4)..<(offset + 8)])
      if type == sampleDependencyType {
        data.replaceSubrange((offset + 4)..<(offset + 8), with: freeType)
      } else if containers.contains(type) {
        try neutralizeSampleDependencyBoxes(
          in: &data,
          start: offset + headerSize,
          end: offset + boxSize
        )
      }
      offset += boxSize
    }
  }

  /// One top-level or child box: where it starts, where its payload starts,
  /// and where it ends.
  struct Box {
    let type: [UInt8]
    let offset: Int
    let headerSize: Int
    let size: Int
    var contentStart: Int { offset + headerSize }
    var end: Int { offset + size }
  }

  /// Parses the boxes tiling `[start, end)` without descending into them.
  private static func boxes(in data: Data, start: Int, end: Int) throws -> [Box] {
    var parsed: [Box] = []
    var offset = start
    while offset < end {
      guard end - offset >= 8 else { throw invalidMp4BoxError() }
      let compactSize = Int(readBigEndianUInt32(data, at: offset))
      var headerSize = 8
      let boxSize: Int
      if compactSize == 1 {
        guard end - offset >= 16 else { throw invalidMp4BoxError() }
        let extendedSize = readBigEndianUInt64(data, at: offset + 8)
        guard extendedSize <= UInt64(Int.max) else { throw invalidMp4BoxError() }
        boxSize = Int(extendedSize)
        headerSize = 16
      } else if compactSize == 0 {
        boxSize = end - offset
      } else {
        boxSize = compactSize
      }
      guard boxSize >= headerSize, offset + boxSize <= end else { throw invalidMp4BoxError() }
      parsed.append(
        Box(
          type: Array(data[(offset + 4)..<(offset + 8)]),
          offset: offset,
          headerSize: headerSize,
          size: boxSize
        )
      )
      offset += boxSize
    }
    return parsed
  }

  /// `timescale` of an `mvhd` or `mdhd` box, nil when it is zero or truncated.
  private static func timescale(in data: Data, of box: Box) throws -> UInt32? {
    guard box.end - box.contentStart >= 4 else { throw invalidMp4BoxError() }
    let offset = box.contentStart + (data[box.contentStart] == 1 ? 20 : 12)
    guard offset + 4 <= box.end else { throw invalidMp4BoxError() }
    let value = readBigEndianUInt32(data, at: offset)
    return value == 0 ? nil : value
  }

  /// `duration` of an `mdhd` box, in its own timescale.
  private static func duration(in data: Data, of box: Box) throws -> Int64? {
    guard box.end - box.contentStart >= 4 else { throw invalidMp4BoxError() }
    if data[box.contentStart] == 1 {
      guard box.contentStart + 32 <= box.end else { throw invalidMp4BoxError() }
      let value = readBigEndianUInt64(data, at: box.contentStart + 24)
      return value <= UInt64(Int64.max) ? Int64(value) : nil
    }
    guard box.contentStart + 20 <= box.end else { throw invalidMp4BoxError() }
    return Int64(readBigEndianUInt32(data, at: box.contentStart + 16))
  }

  /// `handler_type` of an `hdlr` box.
  private static func handlerType(in data: Data, of box: Box) throws -> [UInt8] {
    guard box.contentStart + 12 <= box.end else { throw invalidMp4BoxError() }
    return Array(data[(box.contentStart + 8)..<(box.contentStart + 12)])
  }

  /// [time] in [timescale] ticks, nil when it does not convert exactly enough
  /// to be written as an integer edit.
  private static func convert(_ time: CMTime, to timescale: UInt32) -> Int64? {
    guard timescale <= UInt32(Int32.max) else { return nil }
    let converted = CMTimeConvertScale(
      time,
      timescale: CMTimeScale(timescale),
      method: .roundHalfAwayFromZero
    )
    guard converted.isNumeric else { return nil }
    return converted.value
  }

  private static func writeBigEndianUInt32(_ data: inout Data, at offset: Int, _ value: UInt32) {
    for index in 0..<4 {
      data[offset + index] = UInt8((value >> (8 * (3 - index))) & 0xff)
    }
  }

  private static func writeBigEndianUInt64(_ data: inout Data, at offset: Int, _ value: UInt64) {
    for index in 0..<8 {
      data[offset + index] = UInt8((value >> (8 * (7 - index))) & 0xff)
    }
  }

  private static func readBigEndianUInt32(_ data: Data, at offset: Int) -> UInt32 {
    data[offset..<(offset + 4)].reduce(0) { ($0 << 8) | UInt32($1) }
  }

  private static func readBigEndianUInt64(_ data: Data, at offset: Int) -> UInt64 {
    data[offset..<(offset + 8)].reduce(0) { ($0 << 8) | UInt64($1) }
  }

  private static func invalidMp4BoxError() -> NSError {
    NSError(
      domain: "BuzzVideoTranscode",
      code: 1,
      userInfo: [NSLocalizedDescriptionKey: "Invalid MP4 box structure."]
    )
  }
}

extension Array where Element == MP4Canonicalizer.Box {
  /// The first box of [type], or nil.
  func first(ofType type: String) -> MP4Canonicalizer.Box? {
    let wanted = Array<UInt8>(type.utf8)
    return first { $0.type == wanted }
  }

  /// Every box of [type], in file order.
  func all(ofType type: String) -> [MP4Canonicalizer.Box] {
    let wanted = Array<UInt8>(type.utf8)
    return filter { $0.type == wanted }
  }
}
