import { workspaceRelayMembershipNotice } from "@/features/agents/lib/relayMembership";
import type { ManagedAgent, PresenceLookup } from "@/shared/api/types";
import { normalizePubkey } from "@/shared/lib/pubkey";
import { ManagedAgentRow, RelayMembershipBlock } from "./ManagedAgentRow";

export type AgentGroupRowsProps = {
  agents: ManagedAgent[];
  channelIdToName: Record<string, string>;
  channelsByPubkey: Record<string, { id: string; name: string }[]>;
  logContent: string | null;
  logError: Error | null;
  logLoading: boolean;
  personaLabelsById: Record<string, string>;
  presenceLoaded: boolean;
  presenceLookup: PresenceLookup;
  selectedLogAgentPubkey: string | null;
  onOpenProfile: (pubkey: string) => void;
  onSelectLogAgent: (pubkey: string | null) => void;
};

export function AgentGroupRows({
  agents,
  channelIdToName,
  channelsByPubkey,
  logContent,
  logError,
  logLoading,
  personaLabelsById,
  presenceLoaded,
  presenceLookup,
  selectedLogAgentPubkey,
  onOpenProfile,
  onSelectLogAgent,
}: AgentGroupRowsProps) {
  // A relay that refused the WORKSPACE identity refused it once, for every
  // agent here: same npub, same operator command, same remedy. Rendered per
  // row it produced one identical blocking block per agent for a single
  // problem that is not any agent's. The rows suppress theirs
  // (`ManagedAgentRow`); this renders it once, above them, where a group-wide
  // fact belongs.
  const workspaceNotice = workspaceRelayMembershipNotice(agents);
  return (
    <div className="divide-y divide-border/50 border-t border-border/50">
      {workspaceNotice ? (
        <div className="pt-3">
          <RelayMembershipBlock
            agentCount={workspaceNotice.agentCount}
            notice={workspaceNotice.notice}
          />
        </div>
      ) : null}
      {agents.map((agent) => (
        <ManagedAgentRow
          agent={agent}
          channelIdToName={channelIdToName}
          channelNames={channelsByPubkey[normalizePubkey(agent.pubkey)] ?? []}
          isLogSelected={selectedLogAgentPubkey === agent.pubkey}
          key={agent.pubkey}
          logContent={
            selectedLogAgentPubkey === agent.pubkey ? logContent : null
          }
          logError={selectedLogAgentPubkey === agent.pubkey ? logError : null}
          logLoading={selectedLogAgentPubkey === agent.pubkey && logLoading}
          personaLabelsById={personaLabelsById}
          presenceLoaded={presenceLoaded}
          presenceLookup={presenceLookup}
          onOpenProfile={onOpenProfile}
          onSelectLogAgent={onSelectLogAgent}
        />
      ))}
    </div>
  );
}
