/**
 * Re-engage the relay session before a membership re-check.
 *
 * A `restricted: not a relay member` AUTH rejection latches the relay
 * session terminal: nothing reconnects until explicit user re-engagement.
 * "Try again" on the membership screen is exactly that re-engagement, so
 * the session must be re-opened first; otherwise the re-check throws
 * "session is terminal" instead of asking the relay, and the person who was
 * just added stays on the denied screen until they relaunch the app.
 *
 * A rejected preconnect (still not a member) is swallowed: the re-check that
 * follows reports the denial in its own words and keeps the screen.
 */
export async function reengageRelayForMembershipRetry(
  preconnect: () => Promise<unknown>,
): Promise<void> {
  try {
    await preconnect();
  } catch {
    // The membership re-check decides what to show.
  }
}

/** What "Try again" shows when the re-check could not reach a verdict. */
export type MembershipRetryOutcome =
  | "advanced"
  | "denied"
  | "unreachable"
  | "error";

/**
 * The notice the denied screen shows after a retry that neither advanced
 * nor confirmed the denial. Without it a failed re-check left the screen
 * exactly as it was, which read as "the admin's add did not work".
 */
export function membershipRetryNotice(
  outcome: MembershipRetryOutcome,
): string | null {
  switch (outcome) {
    case "unreachable":
      return "Could not reach the relay to check again. Check your connection and try again.";
    case "error":
      return "The relay returned an error while checking again. Try again in a moment.";
    default:
      return null;
  }
}
