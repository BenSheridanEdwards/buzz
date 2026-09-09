import { AlertTriangle } from "lucide-react";

import type { relayMembershipNotice } from "@/features/agents/lib/relayMembership";
import { cn } from "@/shared/lib/cn";
import { CopyButton } from "./CopyButton";

/**
 * Shown when the desktop could not register the agent on a closed relay
 * (or could not verify it). Gives the operator everything they need in one
 * place: why, the npub, and the exact `buzz-admin` command.
 *
 * This is the only recovery affordance the feature has, so it lives in its
 * own file and is rendered by `UnifiedAgentsSection`, the component the
 * user actually sees. It previously lived inside `ManagedAgentRow`, which nothing
 * in the app imported: an earlier commit deleted every usage of that row and
 * left the file behind, so the npub, the operator command and the grouped
 * notice were all unreachable in the running app and the badge on the card
 * was the only part of them a user could read.
 * `UnifiedAgentsSectionRelayMembership.test.mjs` mounts the live section and
 * asserts the command reaches the DOM, so orphaning it again fails a test
 * rather than shipping.
 *
 * `agentCount` is set only by the group-level render of a user-level
 * refusal, where one block stands in for several agents; the sentence it
 * adds is the only place that number is stated, so the count has one owner
 * like every other label here.
 */
export function RelayMembershipBlock({
  agentCount,
  notice,
}: {
  agentCount?: number;
  notice: NonNullable<ReturnType<typeof relayMembershipNotice>>;
}) {
  const blocked = notice.severity === "blocked";
  return (
    <div
      className={cn(
        // Spacing is the caller's: the section that renders this owns the
        // gap between it and the agent grid.
        "space-y-2 rounded-md border p-2 text-xs",
        blocked
          ? "border-amber-500/40 bg-amber-500/5"
          : "border-border bg-muted/30",
      )}
      data-testid={`managed-agent-relay-membership-${notice.severity}`}
    >
      <div className="flex items-center gap-1 font-medium">
        <AlertTriangle
          aria-hidden="true"
          className={cn(
            "h-3 w-3",
            blocked
              ? "text-amber-600 dark:text-amber-400"
              : "text-muted-foreground",
          )}
        />
        <span>{notice.badge}</span>
      </div>
      {notice.detail ? (
        <p className="text-muted-foreground">{notice.detail}</p>
      ) : null}
      {agentCount && agentCount > 1 ? (
        <p className="text-muted-foreground">
          {`This holds up ${agentCount} agents on this relay. Clearing it clears all of them.`}
        </p>
      ) : null}
      {notice.npub ? (
        <div className="flex items-start gap-1.5">
          <div className="min-w-0 flex-1">
            <div className="text-2xs font-medium text-muted-foreground">
              {notice.npubLabel}
            </div>
            <div className="break-all font-mono">{notice.npub}</div>
          </div>
          <CopyButton
            iconOnly
            label={`Copy ${notice.npubLabel.toLowerCase()}`}
            size="icon-xs"
            value={notice.npub}
            variant="ghost"
          />
        </div>
      ) : null}
      {notice.command ? (
        <div className="flex items-start gap-1.5">
          <div className="min-w-0 flex-1">
            <div className="text-2xs font-medium text-muted-foreground">
              Ask the relay operator to run
            </div>
            <code className="block break-all font-mono">{notice.command}</code>
          </div>
          <CopyButton
            iconOnly
            label="Copy operator command"
            size="icon-xs"
            value={notice.command}
            variant="ghost"
          />
        </div>
      ) : null}
    </div>
  );
}
