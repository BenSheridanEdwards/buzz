import * as React from "react";

import { isVoiceNoteAttachment } from "@/features/messages/lib/audioAttachment";
import type { Channel } from "@/shared/api/types";
import type {
  ImetaEntry,
  VoiceNoteCardContext,
} from "@/shared/ui/markdown/types";

/**
 * Card context for a message that carries a voice note, or undefined when it
 * carries none.
 *
 * The result travels into the Markdown runtime and is compared by identity by
 * `markdownPropsAreEqual`, so it must only change when the card would look
 * different. The conversation kind is derived from the channel list, which is
 * a fresh array on every channel update (unread counts, names, membership);
 * memoising the card on that list would re-render every voice-note row's
 * Markdown each time any channel changed anywhere. The list is collapsed to
 * the `"dm" | "channel"` primitive first, and only that reaches the card.
 */
export function useVoiceNoteCardContext({
  channelId,
  channels,
  imetaByUrl,
  renderTranscript,
  sender,
}: {
  /** The conversation this message belongs to, from a prop or its `h` tag. */
  channelId: string | null;
  channels: readonly Channel[];
  imetaByUrl: ReadonlyMap<string, ImetaEntry> | undefined;
  renderTranscript: (transcript: string) => React.ReactNode;
  sender: string | undefined;
}): VoiceNoteCardContext | undefined {
  const conversation = React.useMemo(
    () =>
      channels.find((channel) => channel.id === channelId)?.channelType === "dm"
        ? ("dm" as const)
        : ("channel" as const),
    [channelId, channels],
  );
  return React.useMemo(() => {
    if (!imetaByUrl) return undefined;
    let hasVoiceNote = false;
    for (const entry of imetaByUrl.values()) {
      if (isVoiceNoteAttachment(entry)) {
        hasVoiceNote = true;
        break;
      }
    }
    if (!hasVoiceNote) return undefined;
    return { conversation, renderTranscript, sender };
  }, [conversation, imetaByUrl, renderTranscript, sender]);
}
