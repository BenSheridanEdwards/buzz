import 'dart:convert';
import 'dart:io';
import 'dart:typed_data';

const _maxTopLevelBoxes = 4096;
const _maxNestedBoxes = 100000;
const _maxBoxDepth = 32;
const _maxMoovBytes = 64 * 1024 * 1024;
const _copyBufferBytes = 1024 * 1024;
const _uint32Max = 0xffffffff;
const _uint64Max = 0x7fffffffffffffff;
const _containerTypes = {
  'moov',
  'trak',
  'mdia',
  'minf',
  'stbl',
  'edts',
  'dinf',
};

final class _Mp4Box {
  final int offset;
  final int size;
  final int headerSize;
  final String type;

  const _Mp4Box({
    required this.offset,
    required this.size,
    required this.headerSize,
    required this.type,
  });
}

/// Maps a byte position in the source file to its position in the output.
///
/// The output keeps every non-`moov` box in source order and inserts the
/// (possibly shrunk) `moov` directly before the first `mdat`, so a position
/// loses the old `moov` if it came after it and gains the new `moov` if it is
/// at or past the first `mdat`. The top-level boxes tile the file exactly, so
/// this holds for any byte, not only box starts.
final class _Relocation {
  final _Mp4Box moov;
  final int firstMdatOffset;
  final int newMoovSize;
  final int fileLength;

  const _Relocation({
    required this.moov,
    required this.firstMdatOffset,
    required this.newMoovSize,
    required this.fileLength,
  });

  int relocate(int position) {
    if (position < 0 || position >= fileLength) {
      throw const FormatException('chunk offset points outside the MP4');
    }
    if (position >= moov.offset && position < moov.offset + moov.size) {
      throw const FormatException('chunk offset points into the moov box');
    }
    var relocated = position;
    if (position > moov.offset) relocated -= moov.size;
    if (position >= firstMdatOffset) relocated += newMoovSize;
    return relocated;
  }
}

/// Rewrites [source] to [destination] with `moov` before the first `mdat`.
///
/// Every `stco`/`co64` entry is remapped through the relocation: the bytes
/// between the first `mdat` and `moov` shift by the new `moov` size, and the
/// bytes after the original `moov` shift by the change in its size, which is
/// zero unless [stripMoovMetadata] dropped boxes. A chunk offset that points
/// into `moov` itself or past the end of the file is rejected. The files must
/// be distinct, and malformed or excessively nested inputs fail closed rather
/// than producing a partially valid upload.
///
/// With [stripMoovMetadata], `meta` and `udta` boxes directly under `moov` are
/// dropped as well. Android's `MediaMuxer` has no switch for the metadata it
/// writes there, and the relay rejects any such box on audio uploads.
Future<void> rewriteMp4ForFastStart(
  File source,
  File destination, {
  bool stripMoovMetadata = false,
}) async {
  if (source.absolute.path == destination.absolute.path) {
    throw const FormatException(
      'MP4 fast-start rewrite requires a distinct destination',
    );
  }

  final input = await source.open();
  try {
    final boxes = await _readTopLevelBoxes(input);
    if (boxes.isEmpty || boxes.first.type != 'ftyp') {
      throw const FormatException('MP4 must start with an ftyp box');
    }
    final moovBoxes = boxes.where((box) => box.type == 'moov').toList();
    if (moovBoxes.length != 1) {
      throw const FormatException('MP4 must contain exactly one moov box');
    }
    final moov = moovBoxes.single;
    final mdatBoxes = boxes.where((box) => box.type == 'mdat').toList();
    final firstMdat = mdatBoxes.isEmpty ? null : mdatBoxes.first;
    if (firstMdat == null) {
      throw const FormatException('MP4 is missing an mdat box');
    }
    if (moov.size > _maxMoovBytes) {
      throw const FormatException('MP4 moov box is too large');
    }

    await input.setPosition(moov.offset);
    var moovBytes = await input.read(moov.size);
    if (moovBytes.length != moov.size) {
      throw const FormatException('truncated MP4 moov box');
    }
    if (stripMoovMetadata) {
      moovBytes = _withoutMoovMetadata(moovBytes, moov.headerSize);
    }
    final relocation = _Relocation(
      moov: moov,
      firstMdatOffset: firstMdat.offset,
      newMoovSize: moovBytes.length,
      fileLength: await input.length(),
    );
    _patchChunkOffsets(
      moovBytes,
      moov.headerSize,
      moovBytes.length,
      relocation,
      0,
      [0],
    );

    final output = await destination.open(mode: FileMode.write);
    try {
      var insertedMoov = false;
      for (final box in boxes) {
        if (box.type == 'moov') continue;
        if (!insertedMoov && box.type == 'mdat') {
          await output.writeFrom(moovBytes);
          insertedMoov = true;
        }
        await _copyRange(input, output, box.offset, box.size);
      }
      if (!insertedMoov) {
        throw const FormatException('MP4 is missing an mdat box');
      }
      await output.flush();
    } finally {
      await output.close();
    }
  } catch (_) {
    try {
      await destination.delete();
    } on FileSystemException {
      // Best-effort cleanup of a partial rewrite.
    }
    rethrow;
  } finally {
    await input.close();
  }
}

const _moovMetadataTypes = {'meta', 'udta'};

/// Returns [moovBytes] without its direct `meta` and `udta` children, with the
/// `moov` size field rewritten to match.
Uint8List _withoutMoovMetadata(Uint8List moovBytes, int headerSize) {
  final end = moovBytes.length;
  final kept = BytesBuilder(copy: false)..add(moovBytes.sublist(0, headerSize));
  var offset = headerSize;
  var boxes = 0;
  while (offset < end) {
    boxes++;
    if (boxes > _maxNestedBoxes) {
      throw const FormatException('MP4 has too many nested boxes');
    }
    final box = _readMemoryBoxHeader(moovBytes, offset, end);
    if (!_moovMetadataTypes.contains(box.type)) {
      kept.add(moovBytes.sublist(offset, offset + box.size));
    }
    offset += box.size;
  }
  if (offset != end) {
    throw const FormatException('MP4 child boxes do not cover their parent');
  }
  final stripped = kept.takeBytes();
  final header = ByteData.sublistView(stripped);
  if (headerSize == 16) {
    header.setUint32(0, 1, Endian.big);
    header.setUint64(8, stripped.length, Endian.big);
  } else {
    header.setUint32(0, stripped.length, Endian.big);
  }
  return stripped;
}

Future<List<_Mp4Box>> _readTopLevelBoxes(RandomAccessFile input) async {
  final length = await input.length();
  final boxes = <_Mp4Box>[];
  var offset = 0;
  while (offset < length) {
    if (boxes.length >= _maxTopLevelBoxes) {
      throw const FormatException('MP4 has too many top-level boxes');
    }
    final box = await _readFileBoxHeader(input, offset, length);
    boxes.add(box);
    offset += box.size;
  }
  if (offset != length) {
    throw const FormatException('MP4 boxes do not cover the file');
  }
  return boxes;
}

Future<_Mp4Box> _readFileBoxHeader(
  RandomAccessFile input,
  int offset,
  int end,
) async {
  if (end - offset < 8) {
    throw const FormatException('truncated MP4 box header');
  }
  await input.setPosition(offset);
  final compactHeader = await input.read(8);
  final compactSize = _readUint32(compactHeader, 0);
  final type = latin1.decode(compactHeader.sublist(4, 8));
  final int headerSize;
  final int size;
  if (compactSize == 1) {
    if (end - offset < 16) {
      throw const FormatException('truncated extended MP4 box header');
    }
    final extended = await input.read(8);
    headerSize = 16;
    size = _readUint64(extended, 0);
  } else if (compactSize == 0) {
    headerSize = 8;
    size = end - offset;
  } else {
    headerSize = 8;
    size = compactSize;
  }
  if (size < headerSize || size > end - offset) {
    throw const FormatException('invalid MP4 box size');
  }
  return _Mp4Box(
    offset: offset,
    size: size,
    headerSize: headerSize,
    type: type,
  );
}

Future<void> _copyRange(
  RandomAccessFile input,
  RandomAccessFile output,
  int offset,
  int size,
) async {
  await input.setPosition(offset);
  var remaining = size;
  while (remaining > 0) {
    final bytes = await input.read(
      remaining < _copyBufferBytes ? remaining : _copyBufferBytes,
    );
    if (bytes.isEmpty) {
      throw const FormatException('truncated MP4 while copying');
    }
    await output.writeFrom(bytes);
    remaining -= bytes.length;
  }
}

void _patchChunkOffsets(
  Uint8List bytes,
  int start,
  int end,
  _Relocation relocation,
  int depth,
  List<int> boxesSeen,
) {
  if (depth > _maxBoxDepth) {
    throw const FormatException('MP4 box nesting is too deep');
  }
  var offset = start;
  while (offset < end) {
    boxesSeen[0]++;
    if (boxesSeen[0] > _maxNestedBoxes) {
      throw const FormatException('MP4 has too many nested boxes');
    }
    final box = _readMemoryBoxHeader(bytes, offset, end);
    final boxEnd = offset + box.size;
    switch (box.type) {
      case 'stco':
        _patchStco(bytes, offset + box.headerSize, boxEnd, relocation);
        break;
      case 'co64':
        _patchCo64(bytes, offset + box.headerSize, boxEnd, relocation);
        break;
      case final type when _containerTypes.contains(type):
        _patchChunkOffsets(
          bytes,
          offset + box.headerSize,
          boxEnd,
          relocation,
          depth + 1,
          boxesSeen,
        );
        break;
    }
    offset = boxEnd;
  }
  if (offset != end) {
    throw const FormatException('MP4 child boxes do not cover their parent');
  }
}

_Mp4Box _readMemoryBoxHeader(Uint8List bytes, int offset, int end) {
  if (end - offset < 8) {
    throw const FormatException('truncated MP4 child box');
  }
  final compactSize = _readUint32(bytes, offset);
  final type = latin1.decode(bytes.sublist(offset + 4, offset + 8));
  final int headerSize;
  final int size;
  if (compactSize == 1) {
    if (end - offset < 16) {
      throw const FormatException('truncated extended MP4 child box');
    }
    headerSize = 16;
    size = _readUint64(bytes, offset + 8);
  } else if (compactSize == 0) {
    headerSize = 8;
    size = end - offset;
  } else {
    headerSize = 8;
    size = compactSize;
  }
  if (size < headerSize || size > end - offset) {
    throw const FormatException('invalid MP4 child box size');
  }
  return _Mp4Box(
    offset: offset,
    size: size,
    headerSize: headerSize,
    type: type,
  );
}

void _patchStco(Uint8List bytes, int start, int end, _Relocation relocation) {
  if (end - start < 8) throw const FormatException('truncated stco box');
  final count = _readUint32(bytes, start + 4);
  if (count > (end - start - 8) ~/ 4) {
    throw const FormatException('invalid stco entry count');
  }
  final data = ByteData.sublistView(bytes);
  for (var index = 0; index < count; index++) {
    final entry = start + 8 + index * 4;
    final adjusted = relocation.relocate(data.getUint32(entry, Endian.big));
    if (adjusted > _uint32Max) {
      throw const FormatException('stco offset overflow');
    }
    data.setUint32(entry, adjusted, Endian.big);
  }
}

void _patchCo64(Uint8List bytes, int start, int end, _Relocation relocation) {
  if (end - start < 8) throw const FormatException('truncated co64 box');
  final count = _readUint32(bytes, start + 4);
  if (count > (end - start - 8) ~/ 8) {
    throw const FormatException('invalid co64 entry count');
  }
  final data = ByteData.sublistView(bytes);
  for (var index = 0; index < count; index++) {
    final entry = start + 8 + index * 8;
    final adjusted = relocation.relocate(data.getUint64(entry, Endian.big));
    if (adjusted > _uint64Max) {
      throw const FormatException('co64 offset overflow');
    }
    data.setUint64(entry, adjusted, Endian.big);
  }
}

int _readUint32(Uint8List bytes, int offset) =>
    ByteData.sublistView(bytes).getUint32(offset, Endian.big);

int _readUint64(Uint8List bytes, int offset) =>
    ByteData.sublistView(bytes).getUint64(offset, Endian.big);
