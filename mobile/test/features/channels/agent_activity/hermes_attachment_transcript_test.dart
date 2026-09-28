import 'dart:convert';
import 'dart:io';

import 'package:buzz/features/channels/agent_activity/observer_models.dart';
import 'package:buzz/features/channels/agent_activity/transcript_builder.dart';
import 'package:buzz/features/channels/agent_activity/transcript_item_widget.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

import '../../../helpers/widget_helpers.dart';

// Mirrors desktop/src/features/agents/ui/hermesAttachmentTranscript.test.mjs so
// both clients show the same transcript for the same Hermes observer frames.

/// Real Hermes-produced frames shared with the desktop reducer tests.
/// `flutter test` runs from `mobile/`, so the repo fixture is one level up.
Map<String, dynamic> _canonicalFixture() => jsonDecode(
  File(
    '../test-fixtures/hermes-attachment/canonical-v1.json',
  ).readAsStringSync(),
);

ObserverFrame _frame(
  int seq,
  Map<String, dynamic> payload, {
  String kind = 'acp_read',
  String? sessionId = 'session',
  String? channelId = 'channel',
  String? turnId = 'buzz-turn',
  int? agentIndex = 0,
}) => ObserverFrame(
  seq: seq,
  timestamp: '2026-09-26T00:00:${seq.toString().padLeft(2, '0')}Z',
  kind: kind,
  agentIndex: agentIndex,
  channelId: channelId,
  sessionId: sessionId,
  turnId: turnId,
  payload: payload,
);

ObserverFrame _text(
  int seq,
  String text, [
  Map<String, dynamic> meta = const {},
]) => _frame(seq, {
  'jsonrpc': '2.0',
  'method': 'session/update',
  'params': {
    'sessionId': 'session',
    '_meta': {
      'turnId': 'canonical-turn',
      'messageId': 'canonical-turn:assistant',
      'kind': 'live',
      'operation': 'append',
      ...meta,
    },
    'update': {
      'sessionUpdate': 'agent_message_chunk',
      'content': {'type': 'text', 'text': text},
    },
  },
});

ObserverFrame _final(
  int seq,
  String text, {
  int part = 0,
  int parts = 1,
  int deliveryId = 17,
}) => _text(seq, text, {
  'kind': 'final',
  'operation': 'replace',
  'part': part,
  'parts': parts,
  'deliveryId': deliveryId,
});

ObserverFrame _receipt(int seq, Map<String, dynamic> params) => _frame(seq, {
  'jsonrpc': '2.0',
  'method': '_hermes/turn_complete',
  'params': {
    'sessionId': 'session',
    'turnId': 'canonical-turn',
    '_meta': {'deliveryId': 22},
    ...params,
  },
});

ObserverFrame _nativeTool(int seq, Map<String, dynamic> update) => _frame(seq, {
  'method': 'session/update',
  'params': {'sessionId': 'session', 'update': update},
});

ObserverFrame _hermesTool(int seq, Map<String, dynamic> update) => _frame(seq, {
  'method': 'session/update',
  'params': {
    'sessionId': 'session',
    '_meta': {
      'kind': 'live',
      'operation': 'merge',
      'turnId': 'canonical-turn',
      'messageId': 'canonical-turn:tool:call',
    },
    'update': update,
  },
});

List<String> _messages(List<TranscriptItem> items) =>
    items.whereType<MessageItem>().map((m) => m.text).toList();

List<LifecycleItem> _lifecycle(List<TranscriptItem> items) =>
    items.whereType<LifecycleItem>().toList();

void main() {
  test(
    'canonical Hermes multipart replay renders one final and one outcome',
    () {
      final fixture = _canonicalFixture();
      final frames = (fixture['frames'] as List).cast<Map<String, dynamic>>();
      final events = [
        for (var i = 0; i < frames.length; i++) _frame(i, frames[i]),
      ];
      final expected = frames
          .where((f) => f['params']['_meta']['kind'] == 'final')
          .map((f) => f['params']['update']['content']['text'] as String)
          .join();

      // Partial parts stage atomically: nothing renders until the last part.
      for (var n = 1; n < frames.length - 1; n++) {
        expect(_messages(buildTranscript(events.take(n).toList())), isEmpty);
      }

      final items = buildTranscript(events);
      expect(_messages(items), [expected]);
      expect(_lifecycle(items).map((i) => i.title), ['Turn completed']);

      // Replaying the whole journal again must not duplicate anything.
      final replayed = buildTranscript([
        ...events,
        for (final e in events) _frame(e.seq + events.length, frames[e.seq]),
      ]);
      expect(_messages(replayed), [expected]);
      expect(_lifecycle(replayed), hasLength(1));
    },
  );

  test('Hermes live text is replaced by its final, not duplicated', () {
    final items = buildTranscript([
      _text(1, 'Commentary'),
      _final(2, 'Final ', part: 0, parts: 2),
    ]);
    expect(_messages(items), ['Commentary']);

    final done = buildTranscript([
      _text(1, 'Commentary'),
      _final(2, 'Final ', part: 0, parts: 2),
      _final(3, 'answer', part: 1, parts: 2, deliveryId: 21),
      // Replay of the same final and a late live delta change nothing.
      _final(4, 'Final ', part: 0, parts: 2),
      _final(5, 'answer', part: 1, parts: 2, deliveryId: 21),
      _text(6, ' late'),
      _text(7, 'old snapshot', {
        'kind': 'snapshot',
        'operation': 'replace',
        'part': 0,
        'parts': 1,
      }),
    ]);
    expect(_messages(done), ['Final answer']);
  });

  test('Hermes live text after a tool opens a segment; final collapses it', () {
    final toolCall = _hermesTool(3, {
      'sessionUpdate': 'tool_call',
      'toolCallId': 'call',
      'title': 'terminal',
      'status': 'in_progress',
      'rawInput': {'command': 'ls'},
    });
    final live = [
      _text(1, 'Let me '),
      _text(2, 'check'),
      toolCall,
      _text(4, 'Found '),
      _text(5, 'it'),
    ];
    List<String> shape(List<TranscriptItem> items) => [
      for (final i in items)
        if (i is MessageItem) i.text else if (i is ToolItem) '[tool]',
    ];
    expect(shape(buildTranscript(live)), [
      'Let me check',
      '[tool]',
      'Found it',
    ]);
    expect(
      shape(
        buildTranscript([
          ...live,
          _final(6, 'Let me check. Found it.', deliveryId: 30),
          _text(7, ' late'),
        ]),
      ),
      ['Let me check. Found it.', '[tool]'],
    );
  });

  test('Hermes durable notices render as their own row, once per delivery', () {
    ObserverFrame notice(int seq, int deliveryId) => _frame(seq, {
      'method': 'session/update',
      'params': {
        'sessionId': 'session',
        '_meta': {'deliveryId': deliveryId, 'replay': true},
        'update': {
          'sessionUpdate': 'agent_message_chunk',
          'content': {'type': 'text', 'text': 'Background job finished'},
        },
      },
    });
    final items = buildTranscript([
      _text(1, 'Answer'),
      notice(2, 17),
      notice(3, 18),
      notice(4, 17),
    ]);
    expect(_messages(items), ['Answer']);
    final notices = _lifecycle(items).where((i) => i.title == 'Notice');
    expect(notices.map((i) => i.text), [
      'Background job finished',
      'Background job finished',
    ]);
  });

  test('Hermes admission renders one prompt row across retries', () {
    ObserverFrame admit(int seq, String turn) => _frame(
      seq,
      {
        'method': '_hermes/turn/admit',
        'params': {
          'sessionId': 'session',
          'admissionId': 'event-id',
          'prompt': [
            {'type': 'text', 'text': '[Buzz event: @mention]\nContent: work'},
          ],
        },
      },
      kind: 'acp_write',
      turnId: turn,
    );
    final items = buildTranscript([admit(1, 'first'), admit(2, 'retry')]);
    final prompts = items.whereType<MessageItem>().where(
      (m) => m.role == 'user',
    );
    expect(prompts.map((m) => m.text), ['work']);
    expect(items.whereType<MetadataItem>().single.title, 'Prompt context');
  });

  test('Hermes turn outcomes render one correlated row per turn', () {
    for (final (params, title, tone) in [
      ({'stopReason': 'end_turn'}, 'Turn completed', LifecycleTone.status),
      ({'stopReason': 'cancelled'}, 'Turn cancelled', LifecycleTone.status),
      ({'error': 'provider broke'}, 'Turn error', LifecycleTone.error),
      (<String, dynamic>{}, 'Turn outcome unknown', LifecycleTone.error),
    ]) {
      final items = buildTranscript([
        _final(1, 'Final'),
        _receipt(2, params),
        _receipt(3, params),
      ]);
      final rows = _lifecycle(items);
      expect(rows, hasLength(1), reason: title);
      expect(rows.single.title, title);
      expect(rows.single.tone, tone);
    }

    final incomplete = buildTranscript([
      _final(1, 'half', part: 0, parts: 2),
      _receipt(2, {'stopReason': 'end_turn'}),
    ]);
    expect(_messages(incomplete), isEmpty);
    expect(_lifecycle(incomplete).single.text, contains('Incomplete'));
    expect(_lifecycle(incomplete).single.tone, LifecycleTone.error);
  });

  test('status-less tool_call_update keeps the call running', () {
    final items = buildTranscript([
      _nativeTool(1, {
        'sessionUpdate': 'tool_call',
        'toolCallId': 'call',
        'title': 'terminal',
        'status': 'in_progress',
      }),
      _nativeTool(2, {
        'sessionUpdate': 'tool_call_update',
        'toolCallId': 'call',
        'rawOutput': 'still working',
      }),
    ]);
    final tool = items.whereType<ToolItem>().single;
    expect(tool.status, ToolStatus.executing);
    expect(tool.isError, isFalse);
    expect(tool.result, 'still working');

    // Late progress cannot reopen a terminal call.
    final done = buildTranscript([
      _nativeTool(1, {
        'sessionUpdate': 'tool_call',
        'toolCallId': 'call',
        'status': 'in_progress',
      }),
      _nativeTool(2, {
        'sessionUpdate': 'tool_call_update',
        'toolCallId': 'call',
        'status': 'completed',
      }),
      _nativeTool(3, {
        'sessionUpdate': 'tool_call_update',
        'toolCallId': 'call',
        'status': 'in_progress',
      }),
    ]);
    expect(done.whereType<ToolItem>().single.status, ToolStatus.completed);
  });

  test('agent_stream_closed retires only that process session tools', () {
    final start = {
      'sessionUpdate': 'tool_call',
      'toolCallId': 'call',
      'title': 'terminal',
      'status': 'in_progress',
    };
    final items = buildTranscript([
      _hermesTool(1, start),
      _frame(2, {
        'method': 'session/update',
        'params': {
          'sessionId': 'other',
          'update': {...start, 'toolCallId': 'other-call'},
        },
      }, sessionId: 'other'),
      _frame(3, {
        'error': 'worker exited',
        'sessionIds': ['session'],
      }, kind: 'agent_stream_closed'),
      // A late terminal frame from the dead process cannot flip it back.
      _hermesTool(4, {
        'sessionUpdate': 'tool_call_update',
        'toolCallId': 'call',
        'status': 'completed',
      }),
    ]);
    final tools = items.whereType<ToolItem>().toList();
    expect(tools.map((t) => t.status), [
      ToolStatus.failed,
      ToolStatus.executing,
    ]);
    expect(tools.first.isError, isTrue);
    expect(tools.first.result, 'Agent process stopped: worker exited');
  });

  test('process-wide agent_stream_closed retires every open call', () {
    final items = buildTranscript([
      _nativeTool(1, {
        'sessionUpdate': 'tool_call',
        'toolCallId': 'call',
        'status': 'in_progress',
      }),
      _frame(
        2,
        {'error': 'crashed', 'processClosed': true, 'sessionIds': []},
        kind: 'agent_stream_closed',
        sessionId: null,
        channelId: null,
      ),
    ]);
    expect(items.whereType<ToolItem>().single.status, ToolStatus.failed);
  });

  test('turn_error and agent_panic render error rows', () {
    final items = buildTranscript([
      _frame(1, {'outcome': 'error', 'error': 'boom'}, kind: 'turn_error'),
      _frame(2, {'error': 'panic'}, kind: 'agent_panic', turnId: 'next'),
    ]);
    expect(_lifecycle(items).map((i) => (i.title, i.text, i.tone)), [
      ('Turn error', 'error: boom', LifecycleTone.error),
      ('Agent error (crash)', 'error: panic', LifecycleTone.error),
    ]);
  });

  testWidgets('outcome rows expose one screen-reader label', (tester) async {
    final handle = tester.ensureSemantics();
    await tester.pumpWidget(
      WidgetHelpers.testable(
        child: Column(
          children: [
            TranscriptItemWidget(
              item: LifecycleItem(
                id: 'error',
                title: 'Turn error',
                text: 'provider broke',
                timestamp: '',
                tone: LifecycleTone.error,
              ),
            ),
            TranscriptItemWidget(
              item: LifecycleItem(
                id: 'done',
                title: 'Turn completed',
                text: 'Turn completed',
                timestamp: '',
              ),
            ),
          ],
        ),
      ),
    );
    expect(find.bySemanticsLabel('Turn error: provider broke'), findsOneWidget);
    expect(find.bySemanticsLabel('Turn completed'), findsOneWidget);
    // Visible text is not a second screen-reader stop, nor duplicated.
    expect(find.bySemanticsLabel(RegExp('provider broke')), findsOneWidget);
    expect(find.text('Turn completed'), findsOneWidget);
    handle.dispose();
  });
}
