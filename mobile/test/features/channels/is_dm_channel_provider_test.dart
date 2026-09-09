import 'package:buzz/features/channels/channel.dart';
import 'package:buzz/features/channels/channels_provider.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:hooks_riverpod/hooks_riverpod.dart';

class _FakeChannelsNotifier extends ChannelsNotifier {
  _FakeChannelsNotifier(this._channels);

  final List<Channel> _channels;

  @override
  Future<List<Channel>> build() async => _channels;
}

Channel _channel(String id, String channelType) => Channel(
  id: id,
  name: id,
  channelType: channelType,
  visibility: 'open',
  description: '',
  createdBy: 'creator',
  createdAt: DateTime(2026),
  memberCount: 2,
);

/// The timeline and the thread view both decide whether a voice-note
/// transcript unfolds on playback from this one lookup, so neither copy can
/// drift from the other.
void main() {
  ProviderContainer containerWith(List<Channel> channels) {
    final container = ProviderContainer(
      overrides: [
        channelsProvider.overrideWith(() => _FakeChannelsNotifier(channels)),
      ],
    );
    addTearDown(container.dispose);
    return container;
  }

  test('a DM channel reads as a DM, a stream channel does not', () async {
    final container = containerWith([
      _channel('dm-1', 'dm'),
      _channel('general', 'stream'),
    ]);
    await container.read(channelsProvider.future);

    expect(container.read(isDmChannelProvider('dm-1')), isTrue);
    expect(container.read(isDmChannelProvider('general')), isFalse);
  });

  test('an unknown channel and an unloaded list are not DMs', () async {
    final container = containerWith([_channel('dm-1', 'dm')]);
    // Before the list resolves nothing is known, so nothing is a DM.
    expect(container.read(isDmChannelProvider('dm-1')), isFalse);

    await container.read(channelsProvider.future);
    expect(container.read(isDmChannelProvider('missing')), isFalse);
  });
}
