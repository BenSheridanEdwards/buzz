import 'dart:convert';
import 'dart:io';
import 'dart:typed_data';

import 'package:buzz/shared/relay/mp4_fast_start.dart';
import 'package:flutter_test/flutter_test.dart';

void main() {
  late Directory tempDirectory;

  setUp(() async {
    tempDirectory = await Directory.systemTemp.createTemp(
      'buzz-faststart-test-',
    );
  });

  tearDown(() async {
    await tempDirectory.delete(recursive: true);
  });

  for (final offsetBox in ['stco', 'co64']) {
    test('moves moov before mdat and patches $offsetBox offsets', () async {
      final ftyp = _box('ftyp', [
        ...ascii.encode('isom'),
        0,
        0,
        0,
        0,
        ...ascii.encode('isom'),
      ]);
      final mdat = _box('mdat', [1, 2, 3, 4]);
      final originalMediaOffset = ftyp.length + 8;
      final offsetPayload = BytesBuilder()
        ..add([0, 0, 0, 0])
        ..add(_uint32(1))
        ..add(
          offsetBox == 'stco'
              ? _uint32(originalMediaOffset)
              : _uint64(originalMediaOffset),
        );
      final sampleTable = _box(offsetBox, offsetPayload.takeBytes());
      final moov = _nestedMoov(sampleTable);
      final sourceBytes = Uint8List.fromList([...ftyp, ...mdat, ...moov]);
      final source = File('${tempDirectory.path}/source.mp4');
      final destination = File('${tempDirectory.path}/output.mp4');
      await source.writeAsBytes(sourceBytes);

      await rewriteMp4ForFastStart(source, destination);

      final output = await destination.readAsBytes();
      expect(_topLevelTypes(output), ['ftyp', 'moov', 'mdat']);
      expect(
        output.sublist(ftyp.length + moov.length + 8),
        equals([1, 2, 3, 4]),
      );

      final typeOffset = _findAscii(output, offsetBox);
      expect(typeOffset, greaterThanOrEqualTo(4));
      final entryOffset = typeOffset + 12;
      final adjusted = offsetBox == 'stco'
          ? _readUint32(output, entryOffset)
          : _readUint64(output, entryOffset);
      expect(adjusted, originalMediaOffset + moov.length);
    });
  }

  test('strips moov-level meta and udta only when asked', () async {
    final ftyp = _box('ftyp', [
      ...ascii.encode('M4A '),
      0,
      0,
      0,
      0,
      ...ascii.encode('isom'),
    ]);
    final mdat = _box('mdat', [9, 9, 9, 9]);
    final originalMediaOffset = ftyp.length + 8;
    final sampleTable = _box('stco', [
      0,
      0,
      0,
      0,
      ..._uint32(1),
      ..._uint32(originalMediaOffset),
    ]);
    final trak = _box(
      'trak',
      _box('mdia', _box('minf', _box('stbl', sampleTable))),
    );
    // MediaMuxer's moov-level metadata: `meta` (com.android.version) and a
    // `udta` box, neither of which the relay accepts on audio uploads.
    final meta = _box('meta', [
      0,
      0,
      0,
      0,
      ..._box('keys', [0, 0, 0, 0]),
    ]);
    final udta = _box('udta', _box('name', ascii.encode('note')));
    final moov = _box('moov', [...trak, ...meta, ...udta]);
    final source = File('${tempDirectory.path}/source.m4a');
    await source.writeAsBytes([...ftyp, ...mdat, ...moov]);

    final kept = File('${tempDirectory.path}/kept.m4a');
    await rewriteMp4ForFastStart(source, kept);
    final keptBytes = await kept.readAsBytes();
    expect(_topLevelTypes(keptBytes), ['ftyp', 'moov', 'mdat']);
    expect(_findAscii(keptBytes, 'meta'), greaterThan(0));
    expect(_findAscii(keptBytes, 'udta'), greaterThan(0));

    final stripped = File('${tempDirectory.path}/stripped.m4a');
    await rewriteMp4ForFastStart(source, stripped, stripMoovMetadata: true);
    final strippedBytes = await stripped.readAsBytes();
    expect(_topLevelTypes(strippedBytes), ['ftyp', 'moov', 'mdat']);
    expect(_findAscii(strippedBytes, 'meta'), -1);
    expect(_findAscii(strippedBytes, 'udta'), -1);
    expect(_findAscii(strippedBytes, 'name'), -1);
    final expectedMoov = _box('moov', trak);
    expect(
      strippedBytes.sublist(ftyp.length, ftyp.length + expectedMoov.length),
      isNot(equals(expectedMoov)),
      reason: 'chunk offsets must be patched for the smaller moov',
    );
    expect(_readUint32(strippedBytes, ftyp.length), expectedMoov.length);
    final entryOffset = _findAscii(strippedBytes, 'stco') + 12;
    expect(
      _readUint32(strippedBytes, entryOffset),
      originalMediaOffset + expectedMoov.length,
    );
    expect(
      strippedBytes.sublist(ftyp.length + expectedMoov.length),
      equals(mdat),
    );
  });

  test(
    'rejects excessive nested box depth and deletes partial output',
    () async {
      final ftyp = _box('ftyp', [
        ...ascii.encode('isom'),
        0,
        0,
        0,
        0,
        ...ascii.encode('isom'),
      ]);
      final mdat = _box('mdat', [1]);
      var nested = _box('stco', [0, 0, 0, 0, ..._uint32(0)]);
      for (var index = 0; index < 34; index++) {
        nested = _box('stbl', nested);
      }
      final source = File('${tempDirectory.path}/deep.mp4');
      final destination = File('${tempDirectory.path}/output.mp4');
      await source.writeAsBytes([...ftyp, ...mdat, ..._box('moov', nested)]);

      await expectLater(
        rewriteMp4ForFastStart(source, destination),
        throwsA(isA<FormatException>()),
      );
      expect(await destination.exists(), isFalse);
    },
  );
}

Uint8List _nestedMoov(List<int> leaf) =>
    _box('moov', _box('trak', _box('mdia', _box('minf', _box('stbl', leaf)))));

Uint8List _box(String type, List<int> payload) => Uint8List.fromList([
  ..._uint32(payload.length + 8),
  ...ascii.encode(type),
  ...payload,
]);

Uint8List _uint32(int value) {
  final bytes = ByteData(4)..setUint32(0, value, Endian.big);
  return bytes.buffer.asUint8List();
}

Uint8List _uint64(int value) {
  final bytes = ByteData(8)..setUint64(0, value, Endian.big);
  return bytes.buffer.asUint8List();
}

int _readUint32(Uint8List bytes, int offset) =>
    ByteData.sublistView(bytes).getUint32(offset, Endian.big);

int _readUint64(Uint8List bytes, int offset) =>
    ByteData.sublistView(bytes).getUint64(offset, Endian.big);

List<String> _topLevelTypes(Uint8List bytes) {
  final types = <String>[];
  var offset = 0;
  while (offset < bytes.length) {
    final size = _readUint32(bytes, offset);
    types.add(ascii.decode(bytes.sublist(offset + 4, offset + 8)));
    offset += size;
  }
  return types;
}

int _findAscii(Uint8List bytes, String value) {
  final needle = ascii.encode(value);
  for (var offset = 0; offset + needle.length <= bytes.length; offset++) {
    var matches = true;
    for (var index = 0; index < needle.length; index++) {
      if (bytes[offset + index] != needle[index]) {
        matches = false;
        break;
      }
    }
    if (matches) return offset;
  }
  return -1;
}
