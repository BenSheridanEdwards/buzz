import type { TranscriptState } from "./agentSessionTranscript";
import type { ObserverEvent, TranscriptItem } from "./agentSessionTypes";
import { asRecord, asString } from "./agentSessionUtils";
import {
  extractPromptBlocks,
  parsePromptBlocks,
} from "./agentSessionTranscriptHelpers";

export interface HermesReplacement {
  parts: number;
  text: Map<number, string>;
}

/** Keep canonical tool identity independent of the current native prompt. */
export function hermesToolPrefix(
  event: ObserverEvent,
  params: Record<string, unknown>,
  channel: string,
  context: { turnId?: string | null; sessionId?: string | null },
): string {
  const meta = asRecord(params._meta);
  const turn = asString(meta.turnId);
  const session = asString(params.sessionId);
  if (
    turn &&
    session &&
    (meta.operation === "merge" || meta.operation === "replace") &&
    (meta.kind === "live" || meta.kind === "snapshot")
  ) {
    context.turnId = turn;
    context.sessionId = session;
    return `tool:hermes:${JSON.stringify([event.agentIndex, channel, session, turn])}:`;
  }
  return `tool:${channel}:`;
}

function identity(event: ObserverEvent, session: string, message: string) {
  return `hermes:${JSON.stringify([event.agentIndex, event.channelId, session, message])}`;
}

function withItem(
  state: TranscriptState,
  item: TranscriptItem,
): TranscriptState {
  const items = [...state.items];
  const index = items.findIndex((existing) => existing.id === item.id);
  if (index < 0) items.push(item);
  else items[index] = item;
  const itemsById = new Map(state.itemsById);
  itemsById.set(item.id, item);
  return {
    ...state,
    items,
    itemsById,
    latestSessionId: item.sessionId ?? state.latestSessionId,
  };
}

function terminal(
  state: TranscriptState,
  event: ObserverEvent,
  params: Record<string, unknown>,
): TranscriptState {
  const session = asString(params.sessionId);
  const turn = asString(params.turnId);
  if (!session || !turn) return state;
  const id = identity(event, session, `receipt:${turn}`);
  const existing = state.itemsById.get(id);
  const partial = state.hermesParts.has(
    `${identity(event, session, `${turn}:assistant`)}:final`,
  );
  const error =
    asString(params.error) ??
    (partial
      ? "Incomplete authoritative final; reconciliation required."
      : null);
  const stop = asString(params.stopReason);
  const title = error
    ? "Turn error"
    : stop === "cancelled"
      ? "Turn cancelled"
      : stop === "end_turn"
        ? "Turn completed"
        : "Turn outcome unknown";
  return withItem(state, {
    id,
    type: "lifecycle",
    renderClass: error || !stop ? "error" : "status",
    title,
    text: error ?? title,
    timestamp: existing?.timestamp ?? event.timestamp,
    channelId: event.channelId,
    sessionId: session,
    turnId: turn,
    acpSource: "_hermes/turn_complete",
  });
}

function admission(
  state: TranscriptState,
  event: ObserverEvent,
  payload: Record<string, unknown>,
): TranscriptState {
  const params = asRecord(payload.params);
  const session = asString(params.sessionId);
  const trigger = asString(params.admissionId);
  if (!session || !trigger) return state;
  const id = identity(event, session, `admission:${trigger}`);
  if (state.itemsById.has(id)) return state;
  const parsed = parsePromptBlocks(extractPromptBlocks(payload));
  let next = withItem(state, {
    id,
    type: "message",
    renderClass: "message",
    role: "user",
    title: parsed.userTitle,
    text: parsed.userText,
    timestamp: event.timestamp,
    channelId: event.channelId,
    sessionId: session,
    turnId: event.turnId,
    authorPubkey: parsed.userPubkey,
    messageId: parsed.userEventId,
    acpSource: "_hermes/turn/admit",
  });
  if (parsed.sections.length)
    next = withItem(next, {
      id: `${id}:context`,
      type: "metadata",
      renderClass: "raw-rail",
      title: "Prompt context",
      sections: parsed.sections,
      timestamp: event.timestamp,
      channelId: event.channelId,
      sessionId: session,
      turnId: event.turnId,
      acpSource: "_hermes/turn/admit",
    });
  return next;
}

function recoveryStatus(
  state: TranscriptState,
  event: ObserverEvent,
  payload: Record<string, unknown>,
): TranscriptState | null {
  const result = asRecord(payload.result);
  const meta = asRecord(result._meta);
  const error = asString(asRecord(payload.error).message);
  const status = asString(result.status);
  const admission = asString(result.admissionId);
  let text: string | null = null;
  if (
    meta.historyTruncated === true ||
    asRecord(meta.activeTurn).snapshotTruncated === true
  )
    text =
      "Restored history or active tool snapshot was truncated; canonical history is the recovery source.";
  if (error?.includes("replay_gap")) text = error;
  if (admission && status && ["unknown", "error", "rejected"].includes(status))
    text =
      asString(result.error) ??
      `Admission ${status}; a new user action is required.`;
  if (!text) return null;
  const session = asString(result.sessionId) ?? event.sessionId ?? "unknown";
  const id = identity(
    event,
    session,
    admission ? `admission-status:${admission}` : `recovery:${event.seq}`,
  );
  return withItem(state, {
    id,
    type: "lifecycle",
    renderClass: "error",
    title: "Attachment recovery",
    text,
    timestamp: event.timestamp,
    channelId: event.channelId,
    sessionId: session,
    turnId: asString(result.turnId),
    acpSource: "hermes:recovery",
  });
}

/** Attachment message identity is independent of Buzz's prompt/continuation IDs. */
export function processHermesTranscriptEvent(
  state: TranscriptState,
  event: ObserverEvent,
): TranscriptState | null {
  const payload = asRecord(event.payload);
  const params = asRecord(payload.params);
  if (event.kind === "acp_write" && payload.method === "_hermes/turn/admit")
    return admission(state, event, payload);
  if (event.kind !== "acp_read") return null;
  if (!payload.method) return recoveryStatus(state, event, payload);
  if (payload.method === "_hermes/turn_complete")
    return terminal(state, event, params);
  if (payload.method !== "session/update") return null;
  const meta = asRecord(params._meta);
  const session = asString(params.sessionId);
  const message = asString(meta.messageId);
  const kind = asString(meta.kind);
  const notice = asRecord(asRecord(params.update).content);
  if (
    session &&
    !kind &&
    Number.isSafeInteger(meta.deliveryId) &&
    typeof notice.text === "string"
  ) {
    const id = identity(event, session, `delivery:${meta.deliveryId}`);
    return withItem(state, {
      id,
      type: "lifecycle",
      renderClass: "status",
      title: "Notice",
      text: notice.text,
      timestamp: state.itemsById.get(id)?.timestamp ?? event.timestamp,
      channelId: event.channelId,
      sessionId: session,
      turnId: null,
      acpSource: "hermes:notice",
    });
  }
  if (
    !session ||
    !message ||
    !kind ||
    !["live", "snapshot", "final", "history"].includes(kind)
  )
    return null;
  const update = asRecord(params.update);
  if (
    update.sessionUpdate !== "agent_message_chunk" &&
    !(kind === "history" && update.sessionUpdate === "user_message_chunk")
  )
    return null;
  const content = asRecord(update.content);
  if (content.type !== "text" || typeof content.text !== "string") return state;
  const id = identity(event, session, message);
  let text = content.text;
  let parts = state.hermesParts;
  const existing = state.itemsById.get(id);
  if (existing?.acpSource === "hermes:final" && kind !== "final") return state;
  if (meta.operation === "replace") {
    const part = meta.part;
    const count = meta.parts;
    if (
      !Number.isInteger(part) ||
      !Number.isInteger(count) ||
      typeof part !== "number" ||
      typeof count !== "number" ||
      part < 0 ||
      count < 1 ||
      count > 64 ||
      part >= count
    )
      return state;
    const groupKey = `${id}:${kind}`;
    const previous = part === 0 ? undefined : parts.get(groupKey);
    if (part !== 0 && (!previous || previous.parts !== count)) return state;
    const group: HermesReplacement = {
      parts: count,
      text: new Map(previous?.text),
    };
    group.text.set(part, text);
    parts = new Map(parts);
    parts.set(groupKey, group);
    if (group.text.size !== count) return { ...state, hermesParts: parts };
    text = Array.from(
      { length: count },
      (_, index) => group.text.get(index) ?? "",
    ).join("");
    parts.delete(groupKey);
  } else if (meta.operation === "append" && kind === "live") {
    text = (existing?.type === "message" ? existing.text : "") + text;
  } else {
    return state;
  }
  if (kind === "history") {
    return withItem(
      { ...state, hermesParts: parts },
      {
        id,
        type: "metadata",
        renderClass: "raw-rail",
        title: "Restored history",
        sections: [
          {
            title:
              update.sessionUpdate === "user_message_chunk"
                ? "User"
                : "Assistant",
            body: text,
          },
        ],
        timestamp: existing?.timestamp ?? event.timestamp,
        channelId: event.channelId,
        sessionId: session,
        turnId: null,
        acpSource: "hermes:history",
      },
    );
  }
  const item: TranscriptItem = {
    id,
    type: "message",
    renderClass: "message",
    role: "assistant",
    title: "Assistant",
    text,
    timestamp: existing?.timestamp ?? event.timestamp,
    channelId: event.channelId,
    sessionId: session,
    turnId: asString(meta.turnId),
    messageId: message,
    authorPubkey: null,
    acpSource: `hermes:${kind}`,
  };
  return withItem({ ...state, hermesParts: parts }, item);
}
