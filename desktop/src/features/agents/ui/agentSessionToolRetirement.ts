import type { ObserverEvent, TranscriptItem } from "./agentSessionTypes";
import { classifyTool } from "./agentSessionToolClassifier";
import { asRecord, asString } from "./agentSessionUtils";

/** Retire only calls belonging to the closed process and original session. */
export function retiredToolUpdates(
  items: readonly TranscriptItem[],
  event: ObserverEvent,
): TranscriptItem[] {
  if (event.agentIndex == null) return [];
  if (!event.sessionId && asRecord(event.payload).processClosed !== true)
    return [];
  const error =
    asString(asRecord(event.payload).error) ?? "Agent process stopped";
  const updates: TranscriptItem[] = [];
  for (const item of items) {
    if (
      item.type !== "tool" ||
      item.agentIndex !== event.agentIndex ||
      (event.sessionId != null && item.sessionId !== event.sessionId) ||
      (event.channelId != null && item.channelId !== event.channelId) ||
      (item.status !== "executing" && item.status !== "pending")
    )
      continue;
    const result = `${item.result}${item.result ? "\n\n" : ""}Agent process stopped: ${error}`;
    const descriptor = classifyTool({ ...item, result, isError: true });
    updates.push({
      ...item,
      result,
      descriptor,
      renderClass: descriptor.renderClass,
      status: "failed",
      isError: true,
      completedAt: event.timestamp,
      acpSource: event.kind,
    });
  }
  return updates;
}
