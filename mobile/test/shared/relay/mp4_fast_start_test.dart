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
    'strip keeps chunk offsets valid when moov already precedes mdat',
    () async {
      // Any faststart file: ffmpeg +faststart, every AVFoundation export. The
      // strip shrinks moov, so mdat moves left and each stco entry must follow.
      final ftyp = _box('ftyp', [
        ...ascii.encode('M4A '),
        0,
        0,
        0,
        0,
        ...ascii.encode('isom'),
      ]);
      final meta = _box('meta', [
        0,
        0,
        0,
        0,
        ..._box('keys', [0, 0, 0, 0]),
      ]);
      // trak/mdia/minf/stbl headers plus a one-entry stco.
      final trakSize = 8 * 4 + 20;
      final moovSize = 8 + trakSize + meta.length;
      final originalMediaOffset = ftyp.length + moovSize + 8;
      final sampleTable = _box('stco', [
        0,
        0,
        0,
        0,
        ..._uint32(1),
        ..._uint32(originalMediaOffset),
      ]);
      final trak = _nestedTrak(sampleTable);
      expect(trak.length, trakSize);
      final moov = _box('moov', [...trak, ...meta]);
      final mdat = _box('mdat', [5, 6, 7, 8]);
      final source = File('${tempDirectory.path}/faststart.m4a');
      await source.writeAsBytes([...ftyp, ...moov, ...mdat]);
      expect(originalMediaOffset, 112, reason: 'the reviewer repro layout');

      final stripped = File('${tempDirectory.path}/stripped.m4a');
      await rewriteMp4ForFastStart(source, stripped, stripMoovMetadata: true);

      final bytes = await stripped.readAsBytes();
      expect(_topLevelTypes(bytes), ['ftyp', 'moov', 'mdat']);
      expect(_findAscii(bytes, 'meta'), -1);
      final entry = _readUint32(bytes, _findAscii(bytes, 'stco') + 12);
      expect(entry, originalMediaOffset - meta.length);
      expect(entry, 88);
      expect(bytes.sublist(entry, entry + 4), [5, 6, 7, 8]);
    },
  );

  test('strip shifts chunk offsets into boxes after a trailing moov', () async {
    // ftyp, mdat, moov(meta), mdat: the second mdat moves by the strip delta
    // even though it never sat between the first mdat and moov.
    final ftyp = _box('ftyp', [
      ...ascii.encode('isom'),
      0,
      0,
      0,
      0,
      ...ascii.encode('isom'),
    ]);
    final firstMdat = _box('mdat', [1, 1]);
    final meta = _box('meta', [
      0,
      0,
      0,
      0,
      ..._box('keys', [0, 0, 0, 0]),
    ]);
    // trak/mdia/minf/stbl headers plus a two-entry stco.
    final trakSize = 8 * 4 + 24;
    final moovSize = 8 + trakSize + meta.length;
    final firstChunk = ftyp.length + 8;
    final secondChunk = ftyp.length + firstMdat.length + moovSize + 8;
    final sampleTable = _box('stco', [
      0,
      0,
      0,
      0,
      ..._uint32(2),
      ..._uint32(firstChunk),
      ..._uint32(secondChunk),
    ]);
    final trak = _nestedTrak(sampleTable);
    expect(trak.length, trakSize);
    final moov = _box('moov', [...trak, ...meta]);
    final secondMdat = _box('mdat', [2, 2, 2]);
    final source = File('${tempDirectory.path}/split.m4a');
    await source.writeAsBytes([...ftyp, ...firstMdat, ...moov, ...secondMdat]);

    final stripped = File('${tempDirectory.path}/stripped.m4a');
    await rewriteMp4ForFastStart(source, stripped, stripMoovMetadata: true);

    final bytes = await stripped.readAsBytes();
    expect(_topLevelTypes(bytes), ['ftyp', 'moov', 'mdat', 'mdat']);
    final entries = _findAscii(bytes, 'stco') + 12;
    final first = _readUint32(bytes, entries);
    final second = _readUint32(bytes, entries + 4);
    expect(first, firstChunk + moovSize - meta.length);
    expect(bytes.sublist(first, first + 2), [1, 1]);
    expect(second, secondChunk - meta.length);
    expect(bytes.sublist(second, second + 3), [2, 2, 2]);

    // Without the strip the trailing mdat does not move at all.
    final kept = File('${tempDirectory.path}/kept.m4a');
    await rewriteMp4ForFastStart(source, kept);
    final keptBytes = await kept.readAsBytes();
    final keptEntries = _findAscii(keptBytes, 'stco') + 12;
    expect(_readUint32(keptBytes, keptEntries), firstChunk + moovSize);
    expect(_readUint32(keptBytes, keptEntries + 4), secondChunk);
  });

  test('ffmpeg faststart output stays playable after the strip', () async {
    // `ffmpeg -c:a aac -movflags +faststart`: ftyp, moov(..., udta/meta/ilst),
    // free, mdat. Proven playable with ffmpeg when the fixture was made; here
    // every chunk offset in the output must address the same bytes it did in
    // the source, which is what a decoder needs.
    final source = File('test/shared/relay/fixtures/faststart-with-meta.m4a');
    final sourceBytes = await source.readAsBytes();
    expect(_topLevelTypes(sourceBytes), ['ftyp', 'moov', 'free', 'mdat']);
    expect(_findAscii(sourceBytes, 'udta'), greaterThan(0));
    final sourceEntries = _stcoEntries(sourceBytes);
    expect(sourceEntries, isNotEmpty);

    final stripped = File('${tempDirectory.path}/stripped.m4a');
    await rewriteMp4ForFastStart(source, stripped, stripMoovMetadata: true);

    final bytes = await stripped.readAsBytes();
    // moov is reinserted directly before the first mdat; free stays put.
    expect(_topLevelTypes(bytes), ['ftyp', 'free', 'moov', 'mdat']);
    expect(_findAscii(bytes, 'udta'), -1);
    expect(_findAscii(bytes, 'meta'), -1);
    expect(_findAscii(bytes, 'ilst'), -1);
    final strippedEntries = _stcoEntries(bytes);
    expect(strippedEntries, hasLength(sourceEntries.length));
    for (var index = 0; index < sourceEntries.length; index++) {
      expect(
        bytes.sublist(strippedEntries[index], strippedEntries[index] + 64),
        sourceBytes.sublist(sourceEntries[index], sourceEntries[index] + 64),
        reason: 'chunk $index must address the same sample bytes',
      );
    }
    expect(strippedEntries.first, lessThan(sourceEntries.first));
  });

  test('rejects chunk offsets that point into moov or past the file', () async {
    final ftyp = _box('ftyp', [
      ...ascii.encode('isom'),
      0,
      0,
      0,
      0,
      ...ascii.encode('isom'),
    ]);
    final mdat = _box('mdat', [1, 2]);
    final moovOffset = ftyp.length + mdat.length;
    for (final target in [moovOffset + 4, moovOffset + 200]) {
      final sampleTable = _box('stco', [
        0,
        0,
        0,
        0,
        ..._uint32(1),
        ..._uint32(target),
      ]);
      final source = File('${tempDirectory.path}/bad-$target.mp4');
      final destination = File('${tempDirectory.path}/out-$target.mp4');
      await source.writeAsBytes([
        ...ftyp,
        ...mdat,
        ..._nestedMoov(sampleTable),
      ]);

      await expectLater(
        rewriteMp4ForFastStart(source, destination),
        throwsA(isA<FormatException>()),
        reason: 'offset $target',
      );
      expect(await destination.exists(), isFalse);
    }
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

Uint8List _nestedMoov(List<int> leaf) => _box('moov', _nestedTrak(leaf));

Uint8List _nestedTrak(List<int> leaf) =>
    _box('trak', _box('mdia', _box('minf', _box('stbl', leaf))));

/// Every `stco` entry in the file, in order, from the first `stco` box.
List<int> _stcoEntries(Uint8List bytes) {
  final start = _findAscii(bytes, 'stco') + 4;
  final count = _readUint32(bytes, start + 4);
  return [
    for (var index = 0; index < count; index++)
      _readUint32(bytes, start + 8 + index * 4),
  ];
}

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
