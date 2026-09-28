part of 'transcript_builder.dart';

// Port of desktop/src/features/agents/ui/hermesAttachmentTranscript.ts.
//
// Hermes gateway attachments carry their own message identity in
// `params._meta` (messageId, kind live/final/snapshot/history, operation
// append/replace/merge, multipart part/parts). That identity is independent of
// Buzz's prompt/continuation IDs, so live text, its authoritative final and any
// journal replay all land on one row. Keep identities, dedupe, replacement and
// terminal semantics in sync with desktop so both clients show the same
// transcript.

const _maxHermesLiveSegments = 256;
const _maxHermesParts = 64;
const _maxSafeInteger = 9007199254740991;

/// Ordered transcript rows plus an id index; rows are replaced in place.
class _TranscriptBuffer {
  final items = <TranscriptItem>[];
  final itemsById = <String, TranscriptItem>{};
  String? latestSessionId;

  void put(TranscriptItem item) {
    final existing = itemsById[item.id];
    if (existing == null) {
      items.add(item);
    } else {
      items[items.indexOf(existing)] = item;
    }
    itemsById[item.id] = item;
  }

  void removePrefixed(String prefix) {
    items.removeWhere((item) => item.id.startsWith(prefix));
    itemsById.removeWhere((key, _) => key.startsWith(prefix));
  }
}

/// A multipart authoritative replacement staged until every part arrives.
class _HermesReplacement {
  final int parts;
  final Map<int, String> text;

  _HermesReplacement(this.parts, this.text);
}

/// Cross-frame Hermes state for one [buildTranscript] pass.
class _HermesState {
  final parts = <String, _HermesReplacement>{};

  /// Stable message identities whose authoritative final has rendered.
  final finals = <String>{};
}

int? _asInteger(dynamic value) {
  if (value is int) return value;
  if (value is double && value.isFinite && value == value.truncateToDouble()) {
    return value.toInt();
  }
  return null;
}

String _hermesIdentity(ObserverFrame event, String session, String message) =>
    'hermes:${jsonEncode([event.agentIndex, event.channelId, session, message])}';

/// Tool-id prefix: canonical Hermes tools are keyed by session and canonical
/// turn so identity survives Buzz re-prompts. Returns the Hermes session and
/// turn so the tool row is attributed to them.
({String prefix, String? sessionId, String? turnId}) _hermesToolPrefix(
  ObserverFrame event,
  Map<String, dynamic> params,
) {
  final meta = _asRecord(params['_meta']);
  final turn = _asString(meta['turnId']);
  final session = _asString(params['sessionId']);
  final operation = meta['operation'];
  final kind = meta['kind'];
  if (turn != null &&
      turn.isNotEmpty &&
      session != null &&
      session.isNotEmpty &&
      (operation == 'merge' || operation == 'replace') &&
      (kind == 'live' || kind == 'snapshot')) {
    final channel = event.channelId ?? 'global';
    final key = jsonEncode([event.agentIndex, channel, session, turn]);
    return (prefix: 'tool:hermes:$key:', sessionId: session, turnId: turn);
  }
  return (prefix: 'tool:', sessionId: null, turnId: null);
}

String _segmentId(String id, int segment) =>
    segment == 0 ? id : '$id:segment:$segment';

/// Newest live segment of a Hermes message (segment 0 is the stable identity).
int _currentSegment(_TranscriptBuffer buf, String id) {
  var segment = 0;
  while (segment < _maxHermesLiveSegments &&
      buf.itemsById.containsKey(_segmentId(id, segment + 1))) {
    segment += 1;
  }
  return segment;
}

/// A tool rendered after a live segment seals it: post-tool text starts a new
/// segment below the tool, mirroring the native path's sealOpenMessages.
bool _sealedByTool(_TranscriptBuffer buf, String itemId) {
  final item = buf.itemsById[itemId];
  if (item == null) return false;
  final index = buf.items.indexOf(item);
  return buf.items.skip(index + 1).any((i) => i is ToolItem);
}

void _hermesTerminal(
  _TranscriptBuffer buf,
  _HermesState state,
  ObserverFrame event,
  Map<String, dynamic> params,
) {
  final session = _asString(params['sessionId']);
  final turn = _asString(params['turnId']);
  if (session == null || session.isEmpty || turn == null || turn.isEmpty) {
    return;
  }
  final id = _hermesIdentity(event, session, 'receipt:$turn');
  final partial = state.parts.containsKey(
    '${_hermesIdentity(event, session, '$turn:assistant')}:final',
  );
  final error =
      _asString(params['error']) ??
      (partial
          ? 'Incomplete authoritative final; reconciliation required.'
          : null);
  final stop = _asString(params['stopReason']);
  final title = error != null
      ? 'Turn error'
      : stop == 'cancelled'
      ? 'Turn cancelled'
      : stop == 'end_turn'
      ? 'Turn completed'
      : 'Turn outcome unknown';
  buf.put(
    LifecycleItem(
      id: id,
      title: title,
      text: error ?? title,
      timestamp: buf.itemsById[id]?.timestamp ?? event.timestamp,
      tone: error != null || stop == null
          ? LifecycleTone.error
          : LifecycleTone.status,
    ),
  );
}

void _hermesAdmission(
  _TranscriptBuffer buf,
  ObserverFrame event,
  Map<String, dynamic> payload,
) {
  final params = _asRecord(payload['params']);
  final session = _asString(params['sessionId']);
  final trigger = _asString(params['admissionId']);
  if (session == null || session.isEmpty || trigger == null) return;
  if (trigger.isEmpty) return;
  final id = _hermesIdentity(event, session, 'admission:$trigger');
  if (buf.itemsById.containsKey(id)) return;
  final parsed = _parsePromptText(_extractPromptText(payload));
  buf.latestSessionId = session;
  buf.put(
    MessageItem(
      id: id,
      role: 'user',
      title: parsed.userTitle,
      text: parsed.userText,
      timestamp: event.timestamp,
    ),
  );
  if (parsed.sections.isNotEmpty) {
    buf.put(
      MetadataItem(
        id: '$id:context',
        title: 'Prompt context',
        sections: parsed.sections,
        timestamp: event.timestamp,
      ),
    );
  }
}

/// Recovery responses (truncated history, replay gaps, ambiguous admission)
/// must stay visible. Returns false when the response carries no warning.
bool _hermesRecoveryStatus(
  _TranscriptBuffer buf,
  ObserverFrame event,
  Map<String, dynamic> payload,
) {
  final result = _asRecord(payload['result']);
  final meta = _asRecord(result['_meta']);
  final error = _asString(_asRecord(payload['error'])['message']);
  final status = _asString(result['status']);
  final admission = _asString(result['admissionId']);
  String? text;
  if (meta['historyTruncated'] == true ||
      _asRecord(meta['activeTurn'])['snapshotTruncated'] == true) {
    text =
        'Restored history or active tool snapshot was truncated; canonical history is the recovery source.';
  }
  if (error != null && error.contains('replay_gap')) text = error;
  if (admission != null &&
      admission.isNotEmpty &&
      status != null &&
      const ['unknown', 'error', 'rejected'].contains(status)) {
    text =
        _asString(result['error']) ??
        'Admission $status; a new user action is required.';
  }
  if (text == null) return false;
  final session =
      _asString(result['sessionId']) ?? event.sessionId ?? 'unknown';
  final id = _hermesIdentity(
    event,
    session,
    admission != null && admission.isNotEmpty
        ? 'admission-status:$admission'
        : 'recovery:${event.seq}',
  );
  buf.latestSessionId = session;
  buf.put(
    LifecycleItem(
      id: id,
      title: 'Attachment recovery',
      text: text,
      timestamp: event.timestamp,
      tone: LifecycleTone.error,
    ),
  );
  return true;
}

/// Applies one Hermes attachment frame. Returns true when the frame was a
/// Hermes frame (consumed, even if it was a no-op), false when the native
/// ACP path should handle it.
bool _processHermesFrame(
  _TranscriptBuffer buf,
  _HermesState state,
  ObserverFrame event,
) {
  final payload = _asRecord(event.payload);
  final params = _asRecord(payload['params']);
  final method = payload['method'];
  if (event.kind == 'acp_write' && method == '_hermes/turn/admit') {
    _hermesAdmission(buf, event, payload);
    return true;
  }
  if (event.kind != 'acp_read') return false;
  if (method == null) return _hermesRecoveryStatus(buf, event, payload);
  if (method == '_hermes/turn_complete') {
    _hermesTerminal(buf, state, event, params);
    return true;
  }
  if (method != 'session/update') return false;

  final meta = _asRecord(params['_meta']);
  final session = _asString(params['sessionId']);
  final message = _asString(meta['messageId']);
  final kind = _asString(meta['kind']);
  final update = _asRecord(params['update']);
  final notice = _asRecord(update['content']);
  final deliveryId = _asInteger(meta['deliveryId']);

  // Durable notices (deliveryId, no kind) are their own row, never assistant
  // text; the delivery id dedupes journal replay.
  if (session != null &&
      session.isNotEmpty &&
      (kind == null || kind.isEmpty) &&
      deliveryId != null &&
      deliveryId.abs() <= _maxSafeInteger &&
      notice['text'] is String) {
    final id = _hermesIdentity(event, session, 'delivery:$deliveryId');
    buf.latestSessionId = session;
    buf.put(
      LifecycleItem(
        id: id,
        title: 'Notice',
        text: notice['text'] as String,
        timestamp: buf.itemsById[id]?.timestamp ?? event.timestamp,
      ),
    );
    return true;
  }

  if (session == null ||
      session.isEmpty ||
      message == null ||
      message.isEmpty ||
      !const ['live', 'snapshot', 'final', 'history'].contains(kind)) {
    return false;
  }
  final updateType = update['sessionUpdate'];
  if (updateType != 'agent_message_chunk' &&
      !(kind == 'history' && updateType == 'user_message_chunk')) {
    return false;
  }
  final content = _asRecord(update['content']);
  if (content['type'] != 'text' || content['text'] is! String) return true;

  final id = _hermesIdentity(event, session, message);
  var text = content['text'] as String;
  var liveId = id;
  final existing = buf.itemsById[id];
  if (state.finals.contains(id) && kind != 'final') return true;

  final operation = meta['operation'];
  if (operation == 'replace') {
    final part = _asInteger(meta['part']);
    final count = _asInteger(meta['parts']);
    if (part == null ||
        count == null ||
        part < 0 ||
        count < 1 ||
        count > _maxHermesParts ||
        part >= count) {
      return true;
    }
    final groupKey = '$id:$kind';
    final previous = part == 0 ? null : state.parts[groupKey];
    if (part != 0 && (previous == null || previous.parts != count)) {
      return true;
    }
    final group = _HermesReplacement(count, {...?previous?.text});
    group.text[part] = text;
    state.parts[groupKey] = group;
    if (group.text.length != count) return true;
    text = [for (var i = 0; i < count; i++) group.text[i] ?? ''].join();
    state.parts.remove(groupKey);
  } else if (operation == 'append' && kind == 'live') {
    final segment = existing != null ? _currentSegment(buf, id) : 0;
    final openId = _segmentId(id, segment);
    final open = buf.itemsById[openId];
    if (open != null && _sealedByTool(buf, openId)) {
      if (segment + 1 >= _maxHermesLiveSegments) return true;
      liveId = _segmentId(id, segment + 1);
    } else {
      liveId = openId;
      text = (open is MessageItem ? open.text : '') + text;
    }
  } else {
    return true;
  }

  // Replacements carry the whole message text; drop superseded live segments.
  if (liveId == id) buf.removePrefixed('$id:segment:');
  buf.latestSessionId = session;

  if (kind == 'history') {
    buf.put(
      MetadataItem(
        id: id,
        title: 'Restored history',
        sections: [
          PromptSection(
            title: updateType == 'user_message_chunk' ? 'User' : 'Assistant',
            body: text,
          ),
        ],
        timestamp: existing?.timestamp ?? event.timestamp,
      ),
    );
    return true;
  }

  if (kind == 'final') state.finals.add(id);
  buf.put(
    MessageItem(
      id: liveId,
      role: 'assistant',
      title: 'Assistant',
      text: text,
      timestamp: buf.itemsById[liveId]?.timestamp ?? event.timestamp,
    ),
  );
  return true;
}

/// Port of desktop agentSessionToolRetirement.ts: when a worker process's
/// stream closes, its unfinished calls (for that session and channel, or all
/// of them on a process-wide close) are final failures.
void _retireTools(_TranscriptBuffer buf, ObserverFrame event) {
  final agentIndex = event.agentIndex;
  if (agentIndex == null) return;
  final payload = _asRecord(event.payload);
  if (event.sessionId == null && payload['processClosed'] != true) return;
  final error = _asString(payload['error']) ?? 'Agent process stopped';
  for (final item in buf.items) {
    if (item is! ToolItem ||
        item.agentIndex != agentIndex ||
        (event.sessionId != null && item.sessionId != event.sessionId) ||
        (event.channelId != null && item.channelId != event.channelId) ||
        (item.status != ToolStatus.executing &&
            item.status != ToolStatus.pending)) {
      continue;
    }
    item.result =
        '${item.result}${item.result.isNotEmpty ? '\n\n' : ''}Agent process stopped: $error';
    item.status = ToolStatus.failed;
    item.isError = true;
    item.retired = true;
  }
}
