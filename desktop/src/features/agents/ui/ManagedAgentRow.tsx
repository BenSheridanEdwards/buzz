import * as React from "react";

import { AlertTriangle, ChevronDown, ChevronRight } from "lucide-react";

import { useAppNavigation } from "@/app/navigation/useAppNavigation";
import { PresenceDot } from "@/features/presence/ui/PresenceBadge";
import { Badge } from "@/shared/ui/badge";
import { AgentStatusBadge } from "@/features/agents/ui/AgentStatusBadge";
import { useAgentWorking } from "@/features/agents/agentWorkingSignal";
import { useOpenAgentActivity } from "@/features/agents/useOpenAgentActivity";
import { formatElapsed } from "@/features/agents/ui/agentSessionUtils";
import { useNow } from "@/shared/lib/useNow";
import type {
  ManagedAgent,
  PresenceLookup,
  PresenceStatus,
} from "@/shared/api/types";
import { cn } from "@/shared/lib/cn";
import { Button } from "@/shared/ui/button";
import { AgentConfigPanel } from "./AgentConfigPanel";
import { friendlyAgentLastError } from "@/features/agents/lib/friendlyAgentLastError";
import {
  type relayMembershipNotice,
  rowRelayMembershipNotice,
} from "@/features/agents/lib/relayMembership";
import { CopyButton } from "./CopyButton";
import { ManagedAgentLogPanel } from "./ManagedAgentLogPanel";
import { PubKey } from "@/shared/ui/PubKey";
import { SubsectionLabel } from "@/shared/ui/PageHeader";
import { resolveModelLabel } from "@/features/agents/lib/formatAgentModelLabel";
import { RestartDiffBadge } from "./RestartDiffBadge";

export function ManagedAgentRow({
  agent,
  channelIdToName,
  channelNames,
  isLogSelected,
  logContent,
  logError,
  logLoading,
  personaLabelsById,
  presenceLoaded,
  presenceLookup,
  onOpenProfile,
  onSelectLogAgent,
}: {
  agent: ManagedAgent;
  channelIdToName: Record<string, string>;
  channelNames: { id: string; name: string }[];
  isLogSelected: boolean;
  logContent: string | null;
  logError: Error | null;
  logLoading: boolean;
  personaLabelsById: Record<string, string>;
  presenceLoaded: boolean;
  presenceLookup: PresenceLookup;
  onOpenProfile: (pubkey: string) => void;
  onSelectLogAgent: (pubkey: string | null) => void;
}) {
  const isLocal = agent.backend.type === "local";
  const runtimeSource =
    agent.backend.type === "provider" ? `Remote (${agent.backend.id})` : null;
  const personaLabel = agent.personaId
    ? (personaLabelsById[agent.personaId] ?? null)
    : null;
  const presenceStatus = presenceLookup[agent.pubkey.trim().toLowerCase()];
  const activeTurns = useAgentWorking(agent.pubkey).channels;
  const activeWorkingChannels = React.useMemo(
    () =>
      activeTurns
        .map(({ channelId, anchorAt }) => ({
          id: channelId,
          name: channelIdToName[channelId] ?? channelId,
          anchorAt,
        }))
        .slice(0, 3),
    [activeTurns, channelIdToName],
  );
  const isWorking = activeWorkingChannels.length > 0;
  const processDetail =
    agent.pid !== null
      ? `PID ${agent.pid}`
      : agent.lastExitCode !== null
        ? `Exit ${agent.lastExitCode}`
        : isLocal
          ? "Ready to launch"
          : "Managed remotely";
  // When the harness recovered a meaningful error string from the agent's
  // log tail (Max's seam in `managed_agents/storage.rs`), promote it to
  // user-visible copy below the process detail. Specifically renders the
  // friendly "Community access denied this agent — check its community membership."
  // for auth failures so the user knows it's a membership thing, not a
  // crash. Generic exits stay verbatim so we don't lie about other failures.
  const friendlyError = friendlyAgentLastError(
    agent.lastError,
    agent.lastErrorCode,
  );
  // `null` for a refusal of the WORKSPACE identity: that one is the group's
  // to render (`AgentGroupRows`), once, rather than once per agent.
  const membershipNotice = rowRelayMembershipNotice(
    agent.pubkey,
    agent.relayMembership,
  );

  return (
    <div
      className={cn(
        "overflow-hidden transition-colors",
        isLogSelected ? "bg-primary/5" : "hover:bg-muted/20",
      )}
      data-testid={`managed-agent-row-${agent.pubkey}`}
    >
      <div className="flex items-start gap-3 px-4 py-3">
        {isLocal ? (
          <button
            aria-expanded={isLogSelected}
            className="-m-1 min-w-0 flex-1 rounded-lg p-1 text-left transition-colors hover:bg-background/40"
            onClick={() =>
              onSelectLogAgent(isLogSelected ? null : agent.pubkey)
            }
            type="button"
          >
            <div className="grid gap-3 lg:grid-cols-[minmax(0,1.8fr)_minmax(120px,0.8fr)_minmax(0,1.1fr)] lg:gap-4">
              <AgentSummary
                activeWorkingChannels={activeWorkingChannels}
                agent={agent}
                channelNames={channelNames}
                isExpandable
                isLogSelected={isLogSelected}
                personaLabel={personaLabel}
                presenceStatus={presenceStatus}
              />
              <StatusBlock
                friendlyError={friendlyError}
                isWorking={isWorking}
                presenceLoaded={presenceLoaded}
                presenceStatus={presenceStatus}
                processDetail={processDetail}
                status={agent.status}
              />
              <RuntimeBlock agent={agent} runtimeSource={runtimeSource} />
            </div>
          </button>
        ) : (
          <div className="min-w-0 flex-1">
            <div className="grid gap-3 lg:grid-cols-[minmax(0,1.8fr)_minmax(120px,0.8fr)_minmax(0,1.1fr)] lg:gap-4">
              <AgentSummary
                activeWorkingChannels={activeWorkingChannels}
                agent={agent}
                channelNames={channelNames}
                isExpandable={false}
                isLogSelected={false}
                personaLabel={personaLabel}
                presenceStatus={presenceStatus}
              />
              <StatusBlock
                friendlyError={friendlyError}
                isWorking={isWorking}
                presenceLoaded={presenceLoaded}
                presenceStatus={presenceStatus}
                processDetail={processDetail}
                status={agent.status}
              />
              <RuntimeBlock agent={agent} runtimeSource={runtimeSource} />
            </div>
          </div>
        )}

        {/* B4: restart badge is a sibling of the expansion button — never
            inside it. TooltipTrigger renders as a <span> (non-interactive),
            so no nested interactive elements are introduced here. */}
        <div className="flex shrink-0 items-start gap-2 lg:pt-0.5">
          {agent.needsRestart ? (
            <RestartDiffBadge
              autoRestartEnabled={agent.autoRestartOnConfigChange}
              restartDiff={agent.restartDiff}
            />
          ) : null}
          <Button
            onClick={() => onOpenProfile(agent.pubkey)}
            size="sm"
            type="button"
            variant="outline"
          >
            Manage
          </Button>
        </div>
      </div>

      {/* Sibling of the expansion button, like the restart badge above: the
          block carries copy buttons, which must never nest inside it. */}
      {membershipNotice ? (
        <RelayMembershipBlock notice={membershipNotice} />
      ) : null}

      {isLocal && isLogSelected ? (
        <div
          className="border-t border-border/60 bg-background/60 px-4 py-4"
          data-testid="managed-agent-log-row"
        >
          <ManagedAgentLogPanel
            error={logError}
            isLoading={logLoading}
            logContent={logContent}
            selectedAgent={agent}
            variant="inline"
          />
          <div className="mt-4 border-t border-border/50 pt-3">
            <p className="mb-2 text-xs font-medium text-muted-foreground">
              Configuration
            </p>
            <AgentConfigPanel pubkey={agent.pubkey} />
          </div>
        </div>
      ) : null}
    </div>
  );
}

function AgentSummary({
  activeWorkingChannels,
  agent,
  channelNames,
  isExpandable,
  isLogSelected,
  personaLabel,
  presenceStatus,
}: {
  activeWorkingChannels: { id: string; name: string; anchorAt: number }[];
  agent: ManagedAgent;
  channelNames: { id: string; name: string }[];
  isExpandable: boolean;
  isLogSelected: boolean;
  personaLabel: string | null;
  presenceStatus: PresenceStatus | undefined;
}) {
  const { goChannel } = useAppNavigation();
  const { openAgentActivity } = useOpenAgentActivity();

  return (
    <div className="min-w-0">
      <div className="flex items-start gap-3">
        {isExpandable ? (
          isLogSelected ? (
            <ChevronDown className="mt-0.5 h-4 w-4 shrink-0 text-muted-foreground" />
          ) : (
            <ChevronRight className="mt-0.5 h-4 w-4 shrink-0 text-muted-foreground" />
          )
        ) : (
          <span className="mt-0.5 h-4 w-4 shrink-0" />
        )}
        {presenceStatus ? (
          <PresenceDot className="mt-1 shrink-0" status={presenceStatus} />
        ) : (
          <span className="mt-1 h-2 w-2 shrink-0" />
        )}
        <div className="min-w-0 flex-1">
          <div className="flex flex-wrap items-center gap-2">
            <p className="truncate font-medium text-foreground">{agent.name}</p>
            {personaLabel ? (
              <Badge variant="secondary">{personaLabel}</Badge>
            ) : null}
            <AgentOriginBadge agent={agent} />
            {agent.personaOrphaned ? (
              <Badge className="gap-1" variant="warning">
                <AlertTriangle className="h-3 w-3" />
                Configuration missing
              </Badge>
            ) : null}
            {agent.personaOutOfDate ? (
              <Badge className="gap-1" variant="warning">
                <AlertTriangle className="h-3 w-3" />
                Out of date
              </Badge>
            ) : null}
          </div>
          <div className="mt-1 flex flex-wrap items-center gap-x-3 gap-y-1 text-xs text-muted-foreground">
            <PubKey pubkey={agent.pubkey} />
            {agent.backend.type === "local" ? (
              <span>
                {agent.startOnAppLaunch ? "Auto-start" : "Manual start"}
              </span>
            ) : (
              <span>Remote deployment</span>
            )}
          </div>
          {agent.personaOrphaned ? (
            <p className="mt-1.5 text-xs text-amber-600 dark:text-amber-400">
              This agent's configuration is missing — it may still be syncing or
              was deleted on another device.
            </p>
          ) : agent.needsRestart ? (
            <p className="mt-1.5 text-xs text-amber-600 dark:text-amber-400">
              Configuration changed since this agent started. Restart to apply
              it.
            </p>
          ) : null}
          {agent.personaOutOfDate ? (
            <p className="mt-1.5 text-xs text-amber-600 dark:text-amber-400">
              Template updated since this agent was created. Respawn to apply
              the new configuration.
            </p>
          ) : null}
          {channelNames.length > 0 ? (
            <div className="mt-1.5 flex flex-wrap items-center gap-1.5">
              {channelNames.map((channel) => (
                <Badge
                  className="cursor-pointer normal-case tracking-normal hover:opacity-80"
                  key={channel.id}
                  variant="outline"
                  onClick={(e) => {
                    e.stopPropagation();
                    void goChannel(channel.id);
                  }}
                >
                  # {channel.name}
                </Badge>
              ))}
            </div>
          ) : null}
          {activeWorkingChannels.length > 0 ? (
            <div className="mt-1.5 flex flex-wrap items-center gap-1.5">
              {activeWorkingChannels.map((channel) => (
                <WorkingBadge
                  key={`working-${channel.id}`}
                  channelId={channel.id}
                  name={channel.name}
                  anchorAt={channel.anchorAt}
                  // Deep-link straight into the agent's activity pane in the
                  // working channel, not just the channel timeline.
                  onNavigate={(channelId) =>
                    openAgentActivity(agent.pubkey, { channelId })
                  }
                />
              ))}
            </div>
          ) : null}
        </div>
      </div>
    </div>
  );
}

function WorkingBadge({
  channelId,
  name,
  anchorAt,
  onNavigate,
}: {
  channelId: string;
  name: string;
  anchorAt: number;
  onNavigate: (channelId: string) => void;
}) {
  // The 1s tick lives here, at the leaf, so only visible working badges
  // re-render each second — idle rows never mount this hook.
  const now = useNow(1000);

  return (
    <Badge
      className="cursor-pointer motion-safe:animate-pulse normal-case tracking-normal hover:opacity-80"
      variant="default"
      onClick={(e) => {
        e.stopPropagation();
        onNavigate(channelId);
      }}
    >
      Working in #{name} · {formatElapsed(now - anchorAt)}
    </Badge>
  );
}

/**
 * Deliberately carries no relay-membership line: `RelayMembershipBlock`
 * below is the single owner of that label in this row. Rendering it here too
 * read the same sentence twice to a screen reader walking the card.
 */
function StatusBlock({
  friendlyError,
  isWorking,
  presenceLoaded,
  presenceStatus,
  processDetail,
  status,
}: {
  friendlyError: ReturnType<typeof friendlyAgentLastError>;
  isWorking: boolean;
  presenceLoaded: boolean;
  presenceStatus: PresenceStatus | undefined;
  processDetail: string;
  status: ManagedAgent["status"];
}) {
  return (
    <div className="space-y-1 lg:pt-0.5">
      <SubsectionLabel className="lg:hidden">Status</SubsectionLabel>
      <AgentStatusBadge
        isWorking={isWorking}
        presenceLoaded={presenceLoaded}
        presenceStatus={presenceStatus}
        status={status}
      />
      <p className="text-xs text-muted-foreground">{processDetail}</p>
      {friendlyError ? (
        <p
          className={cn(
            "text-xs",
            friendlyError.severity === "denied"
              ? "text-destructive"
              : "text-muted-foreground",
          )}
          data-testid="managed-agent-last-error"
        >
          {friendlyError.copy}
        </p>
      ) : null}
    </div>
  );
}

/**
 * Shown when the desktop could not register the agent on a closed relay
 * (or could not verify it). Gives the operator everything they need in one
 * place: why, the npub, and the exact `buzz-admin` command.
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
        "mx-4 mb-3 space-y-2 rounded-md border p-2 text-xs",
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

function RuntimeBlock({
  agent,
  runtimeSource,
}: {
  agent: ManagedAgent;
  runtimeSource: string | null;
}) {
  return (
    <div className="space-y-1 lg:pt-0.5">
      <SubsectionLabel className="lg:hidden">Runtime</SubsectionLabel>
      <p className="truncate font-mono text-xs text-foreground">
        {agent.agentCommand}
      </p>
      {runtimeSource || agent.model ? (
        <div className="flex flex-wrap items-center gap-x-3 gap-y-1 text-xs text-muted-foreground">
          {runtimeSource ? <span>{runtimeSource}</span> : null}
          {agent.model ? (
            <span>{resolveModelLabel(agent.model, null, agent.provider)}</span>
          ) : null}
        </div>
      ) : null}
    </div>
  );
}

function AgentOriginBadge({ agent }: { agent: ManagedAgent }) {
  return (
    <Badge variant="outline">
      {agent.backend.type === "local" ? "Local" : "Remote"}
    </Badge>
  );
}
