import Foundation

/// A deliberately independent ISO-BMFF reader for the checks.
///
/// `MP4Canonicalizer` has its own box walker; asserting against that one would
/// let a parsing bug confirm itself. This reader is written from the spec and
/// used only here.
enum BoxReader {
  struct Node {
    let type: String
    let start: Int
    let payloadStart: Int
    let end: Int
    let children: [Node]

    func child(_ type: String) -> Node? { children.first { $0.type == type } }

    func descendant(_ path: [String]) -> Node? {
      var node: Node? = self
      for step in path {
        node = node?.child(step)
      }
      return node
    }

    /// Every node of [type] in this subtree, self included.
    func all(_ type: String) -> [Node] {
      (self.type == type ? [self] : []) + children.flatMap { $0.all(type) }
    }
  }

  /// Box types whose payload is a list of child boxes.
  private static let containers: Set<String> = [
    "moov", "trak", "mdia", "minf", "stbl", "edts", "dinf", "udta", "ilst", "----",
  ]

  static func parse(_ data: [UInt8]) -> [Node] {
    parse(data, from: 0, to: data.count)
  }

  private static func parse(_ data: [UInt8], from start: Int, to end: Int) -> [Node] {
    var nodes: [Node] = []
    var offset = start
    while offset + 8 <= end {
      var size = Int(be32(data, offset))
      var headerSize = 8
      if size == 1 {
        guard offset + 16 <= end else { break }
        size = Int(be64(data, offset + 8))
        headerSize = 16
      } else if size == 0 {
        size = end - offset
      }
      guard size >= headerSize, offset + size <= end else { break }
      let type = String(bytes: data[(offset + 4)..<(offset + 8)], encoding: .isoLatin1) ?? "????"
      // `meta` is a full box: four bytes of version and flags precede its
      // children.
      let payloadStart = offset + headerSize + (type == "meta" ? 4 : 0)
      let children =
        (containers.contains(type) || type == "meta")
        ? parse(data, from: payloadStart, to: offset + size) : []
      nodes.append(
        Node(
          type: type,
          start: offset,
          payloadStart: payloadStart,
          end: offset + size,
          children: children
        )
      )
      offset += size
    }
    return nodes
  }

  static func be32(_ data: [UInt8], _ offset: Int) -> UInt32 {
    data[offset..<(offset + 4)].reduce(0) { ($0 << 8) | UInt32($1) }
  }

  static func be64(_ data: [UInt8], _ offset: Int) -> UInt64 {
    data[offset..<(offset + 8)].reduce(0) { ($0 << 8) | UInt64($1) }
  }

  /// The one edit-list entry of [elst], as `(segmentDuration, mediaTime)`.
  static func singleEditListEntry(_ data: [UInt8], _ elst: Node) -> (Int64, Int64)? {
    guard elst.payloadStart + 8 <= elst.end, be32(data, elst.payloadStart + 4) == 1 else {
      return nil
    }
    let entry = elst.payloadStart + 8
    if data[elst.payloadStart] == 1 {
      guard entry + 20 <= elst.end else { return nil }
      return (Int64(bitPattern: be64(data, entry)), Int64(bitPattern: be64(data, entry + 8)))
    }
    guard entry + 12 <= elst.end else { return nil }
    return (Int64(be32(data, entry)), Int64(Int32(bitPattern: be32(data, entry + 4))))
  }

  /// `(timescale, duration)` of an `mvhd` or `mdhd` box.
  static func timescaleAndDuration(_ data: [UInt8], _ box: Node) -> (Int64, Int64)? {
    if data[box.payloadStart] == 1 {
      guard box.payloadStart + 32 <= box.end else { return nil }
      return (
        Int64(be32(data, box.payloadStart + 20)),
        Int64(bitPattern: be64(data, box.payloadStart + 24))
      )
    }
    guard box.payloadStart + 20 <= box.end else { return nil }
    return (Int64(be32(data, box.payloadStart + 12)), Int64(be32(data, box.payloadStart + 16)))
  }

  /// `handler_type` of an `hdlr` box.
  static func handlerType(_ data: [UInt8], _ box: Node) -> String? {
    guard box.payloadStart + 12 <= box.end else { return nil }
    return String(bytes: data[(box.payloadStart + 8)..<(box.payloadStart + 12)], encoding: .isoLatin1)
  }

  /// The AAC encoder priming recorded in `udta/meta/ilst/----/iTunSMPB`.
  ///
  /// The atom's value is ASCII hex fields: a reserved word, then the priming
  /// frame count, the remainder padding, and the valid sample count.
  static func iTunSMPBPriming(_ data: [UInt8]) -> Int64? {
    let marker = Array("iTunSMPB".utf8)
    guard let start = indexOf(marker, in: data) else { return nil }
    let text = String(bytes: data[start..<min(start + 256, data.count)], encoding: .isoLatin1) ?? ""
    let fields = text.split(whereSeparator: { !$0.isHexDigit })
      .map(String.init)
      .filter { $0.count >= 8 }
    guard fields.count >= 2 else { return nil }
    return Int64(fields[1], radix: 16)
  }

  private static func indexOf(_ needle: [UInt8], in haystack: [UInt8]) -> Int? {
    guard needle.count <= haystack.count else { return nil }
    for start in 0...(haystack.count - needle.count)
    where Array(haystack[start..<(start + needle.count)]) == needle {
      return start
    }
    return nil
  }
}
