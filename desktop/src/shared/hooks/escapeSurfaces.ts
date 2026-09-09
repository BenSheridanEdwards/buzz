/**
 * Tracks closable foreground surfaces currently listening for Escape.
 *
 * Escape has app-wide meaning (mark channel read) *and* surface-local meaning
 * (close the panel above the channel, discard the recording in the composer).
 * Window listeners fire in registration order, so the app-level shortcut,
 * registered at mount, would otherwise always win the key over a panel that
 * opened later. Instead of racing, background shortcuts ask "is any closable
 * surface open?" and yield.
 *
 * Registration order is not a priority order either: a thread panel mounts
 * before the recorder its composer later opens, and a panel opened over a
 * live recording arrives after it. So surfaces form a **stack** and only the
 * topmost one acts; the rest stay registered and take over again as the
 * surfaces above them close. Without the stack the first-registered listener
 * ate the key and closed the panel out from under a live recording.
 *
 * Nested controls (autocomplete, edit mode) still take priority over the
 * surfaces themselves: they handle Escape on the element and mark it
 * `defaultPrevented`, which every surface listener already respects.
 */
type EscapeSurfaceToken = { readonly id: number };

const surfaceStack: EscapeSurfaceToken[] = [];
let nextSurfaceId = 1;

/** A registered surface's claim on Escape. */
export type EscapeSurface = {
  /** True only while this surface is the newest one still registered. */
  isTopmost: () => boolean;
  /**
   * Gives up the claim. Idempotent: extra calls are ignored so a double
   * cleanup cannot corrupt the stack.
   */
  release: () => void;
};

/** True while at least one closable surface is listening for Escape. */
export function hasActiveEscapeSurface(): boolean {
  return surfaceStack.length > 0;
}

/** Registers a closable surface on top of the stack. */
export function acquireEscapeSurface(): EscapeSurface {
  const token: EscapeSurfaceToken = { id: nextSurfaceId };
  nextSurfaceId += 1;
  surfaceStack.push(token);
  return {
    isTopmost: () => surfaceStack.at(-1) === token,
    release: () => {
      const index = surfaceStack.indexOf(token);
      if (index === -1) return;
      surfaceStack.splice(index, 1);
    },
  };
}
