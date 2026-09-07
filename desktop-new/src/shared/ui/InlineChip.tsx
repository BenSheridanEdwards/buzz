import { IconLink, IconMessageCircle } from "@tabler/icons-react";
import { useSyncExternalStore } from "react";

import type { ChipAddress, ChipKind } from "@/shared/chips/address";
import {
  CHIP_KIND_TRIGGER,
  type ChipFace,
  chipFaces,
} from "@/shared/chips/faceResolver";

/**
 * A reference to a person, agent, channel, message, or link, shown inline in a
 * sentence.
 *
 * One component serves both the composer and the conversation. Two
 * implementations of one face drift, and a reference that looks different
 * depending on whether it is being written or being read is the kind of
 * disconnected feature this client exists to avoid.
 *
 * A chip is an object, not styled text: the caret cannot sit inside it, one
 * deletion removes it whole, and it carries its identity rather than its name.
 * The label is resolved from the address at render time, so a rename updates
 * every chip without editing anyone's draft.
 */

/**
 * Icons only for the kinds a reader cannot identify from the label.
 *
 * A person and an agent are introduced by `@`, a channel by `#` — those sigils
 * are the identification, so adding a glyph would say the same thing twice. A
 * message and a link have no such convention.
 */
const KIND_ICON: Partial<Record<ChipKind, typeof IconLink>> = {
  message: IconMessageCircle,
  link: IconLink,
};

/** Subscribes to face changes so a rename repaints without a document edit. */
function useChipFace(address: ChipAddress): ChipFace {
  return useSyncExternalStore(
    (listener) => chipFaces.subscribe(listener),
    () => chipFaces.get(address),
  );
}

export function InlineChip({
  address,
  interactive = true,
  onActivate,
}: {
  address: ChipAddress;
  /**
   * Whether the chip is a control. Read-only contexts pass false so a chip
   * does not claim an interactive screen-reader stop it cannot honour.
   */
  interactive?: boolean;
  onActivate?: (address: ChipAddress) => void;
}) {
  const face = useChipFace(address);
  const Icon = KIND_ICON[address.kind];
  const trigger = CHIP_KIND_TRIGGER[address.kind];

  const content = (
    <>
      {Icon ? <Icon className="inline-chip-icon" aria-hidden="true" /> : null}
      <span className="inline-chip-label">
        {trigger}
        {face.label}
      </span>
    </>
  );

  // A chip carries one accessible name that states what it refers to. Its icon
  // is decorative — the label already says it, and a second owner of the same
  // name produces a duplicate screen-reader stop.
  //
  // An unresolved face has no name to announce, and announcing an abbreviated
  // identity tells a screen-reader user nothing. Say the kind is unresolved
  // instead; the visible fallback is a recognition aid, not a name.
  const accessibleName = face.resolved
    ? `${accessibleKind(address.kind)} ${face.label}`
    : `Unresolved ${accessibleKind(address.kind).toLowerCase()}`;

  const state = face.resolved ? undefined : "unresolved";

  // An unresolved reference has nothing to open, so it is never a control
  // however it was asked for — a chip that looks actionable and does nothing
  // is worse than one that looks inert.
  if (!interactive || !face.resolved) {
    return (
      <span
        className="inline-chip"
        data-kind={address.kind}
        data-state={state}
        aria-label={accessibleName}
        role="img"
      >
        {content}
      </span>
    );
  }

  return (
    <button
      type="button"
      className="inline-chip"
      data-kind={address.kind}
      aria-label={accessibleName}
      // Keeps the caret in the editor. A control that takes focus moves the
      // caret out and silently breaks every keyboard behaviour after it.
      onMouseDown={(event) => event.preventDefault()}
      onClick={() => onActivate?.(address)}
    >
      {content}
    </button>
  );
}

function accessibleKind(kind: ChipKind): string {
  if (kind === "channel") return "Channel";
  if (kind === "agent") return "Agent";
  if (kind === "message") return "Message";
  if (kind === "link") return "Link";
  return "Person";
}
