import * as React from "react";

import { acquireEscapeSurface } from "@/shared/hooks/escapeSurfaces";

/**
 * Calls `onEscape` when the Escape key is pressed, unless the event
 * was already handled (`defaultPrevented`) — so nested controls
 * (autocomplete, edit mode) that claim Escape on the element always win.
 *
 * The surface is registered with `escapeSurfaces` while enabled, so
 * app-level Escape shortcuts (mark channel read) know to yield, and so that
 * only the **topmost** surface acts: a thread panel registers before the
 * recorder its composer later opens, and a panel opened over a live
 * recording registers after it. Whichever came last owns the key.
 *
 * Pass `enabled: false` to skip registering the listener entirely.
 */
export function useEscapeKey(
  onEscape: () => void,
  enabled: boolean = true,
  options: {
    /**
     * Listen in the capture phase. A surface that covers document-level
     * layers (Radix's dismissable layers listen on `document` in capture)
     * claims the key before them; window capture runs first.
     */
    capture?: boolean;
    /** Decline the key for events a nested control still owns. */
    shouldIgnore?: (event: KeyboardEvent) => boolean;
  } = {},
) {
  const { capture = false } = options;
  const shouldIgnoreRef = React.useRef(options.shouldIgnore);
  shouldIgnoreRef.current = options.shouldIgnore;
  React.useEffect(() => {
    if (!enabled) return;
    const surface = acquireEscapeSurface();
    function handleKeyDown(event: KeyboardEvent) {
      if (event.key !== "Escape" || event.defaultPrevented) return;
      if (!surface.isTopmost()) return;
      if (shouldIgnoreRef.current?.(event)) return;
      event.preventDefault();
      onEscape();
    }
    window.addEventListener("keydown", handleKeyDown, { capture });
    return () => {
      window.removeEventListener("keydown", handleKeyDown, { capture });
      surface.release();
    };
  }, [capture, enabled, onEscape]);
}
