import type { TranscriptItem } from "../agentSessionTypes";

/**
 * Non-terminal / failed state label for a tool row. Deliberately plain text:
 * one polite live region per row would announce every status flip across the
 * whole transcript. Inside a `<summary>` it becomes part of the row's name.
 */
export function ToolStatusBadge({
  item,
}: {
  item: Pick<Extract<TranscriptItem, { type: "tool" }>, "isError" | "status">;
}) {
  const failed = item.isError || item.status === "failed";
  const label = failed
    ? "Failed"
    : item.status === "pending"
      ? "Pending"
      : item.status === "executing"
        ? "Running"
        : null;
  if (!label) return null;
  return (
    <span
      className={
        failed
          ? "shrink-0 text-xs text-destructive"
          : "shrink-0 text-xs text-muted-foreground"
      }
      data-testid="transcript-tool-status"
    >
      {label}
    </span>
  );
}
