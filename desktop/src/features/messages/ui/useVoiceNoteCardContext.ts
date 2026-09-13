import * as React from "react";

import { isVoiceNoteAttachment } from "@/features/messages/lib/audioAttachment";
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
 * different: the sender's name, whether the viewer is that sender, and the
 * transcript renderer. Nothing here depends on the channel list, so a channel
 * update elsewhere (unread counts, names, membership) never re-renders every
 * voice-note row's Markdown.
 */
export function useVoiceNoteCardContext({
  imetaByUrl,
  ownNote,
  renderTranscript,
  sender,
}: {
  imetaByUrl: ReadonlyMap<string, ImetaEntry> | undefined;
  /** The viewer recorded this note. */
  ownNote: boolean;
  renderTranscript: (transcript: string) => React.ReactNode;
  sender: string | undefined;
}): VoiceNoteCardContext | undefined {
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
    return { ownNote, renderTranscript, sender };
  }, [imetaByUrl, ownNote, renderTranscript, sender]);
}
