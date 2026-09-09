import * as React from "react";
import { ChevronDown } from "lucide-react";

import { cn } from "@/shared/lib/cn";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuRadioGroup,
  DropdownMenuRadioItem,
  DropdownMenuTrigger,
} from "@/shared/ui/dropdown-menu";
import {
  type PersonaDropdownOption,
  PERSONA_FIELD_CONTROL_CLASS,
  PERSONA_FIELD_SHELL_CLASS,
} from "./agentConfigOptions";

export function PersonaDropdownField({
  contentClassName,
  describedBy,
  disabled,
  id,
  onValueChange,
  options,
  placeholder,
  readOnly,
  value,
}: {
  contentClassName?: string;
  /** Space-separated ids of the help/error text describing this control. */
  describedBy?: string;
  disabled?: boolean;
  id: string;
  onValueChange: (value: string) => void;
  options: readonly PersonaDropdownOption[];
  placeholder: string;
  /**
   * Inert, but still focusable, so a screen reader reaches the control and the
   * text explaining why it cannot be changed. Prefer this over `disabled` for
   * a value another surface owns: a `disabled` button is skipped by the tab
   * order and its `aria-describedby` is never announced.
   *
   * Inert here means inert on every modality, not just the keyboard: see the
   * `onOpenChange` guard below.
   */
  readOnly?: boolean;
  value: string;
}) {
  const [open, setOpen] = React.useState(false);
  const isInert = disabled === true || readOnly === true;
  const selectedOption = options.find((option) => option.value === value);

  return (
    <div className={PERSONA_FIELD_SHELL_CLASS}>
      <DropdownMenu
        modal={false}
        onOpenChange={(next) => {
          // Guard the state transition, not the events that cause it. Radix
          // opens this menu from four places on the trigger (pointerdown,
          // ArrowDown, Enter and Space) and every one of them funnels through
          // this controlled setter, so one check covers every modality and a
          // modality added later cannot slip past an event allowlist. An
          // `onClick` guard in particular never worked: Radix has already
          // toggled on pointerdown by the time click fires.
          //
          // Closing is never blocked, so a menu opened before the field turned
          // inert stays dismissable. The menu's items only exist while it is
          // open, so this is also the only way a pick can reach
          // `onValueChange`: no second guard is needed there, and one that
          // could never fire would be unfalsifiable (AGENTS.md rule 3).
          if (next && isInert) return;
          setOpen(next);
        }}
        open={open}
      >
        <DropdownMenuTrigger asChild>
          <button
            aria-describedby={describedBy}
            aria-disabled={isInert || undefined}
            className={cn(
              "flex h-11 w-full items-center justify-between gap-3 px-3 py-2 text-left text-sm leading-6",
              PERSONA_FIELD_CONTROL_CLASS,
              isInert && "cursor-default opacity-60",
            )}
            disabled={disabled}
            id={id}
            type="button"
          >
            <span
              className={cn(
                "min-w-0 flex-1 truncate",
                !selectedOption && "text-muted-foreground/55",
              )}
            >
              {selectedOption?.label ?? placeholder}
            </span>
            <ChevronDown className="h-4 w-4 shrink-0 text-muted-foreground/60" />
          </button>
        </DropdownMenuTrigger>
        <DropdownMenuContent
          align="start"
          className={cn("overflow-hidden", contentClassName)}
          onCloseAutoFocus={(event) => event.preventDefault()}
          sideOffset={5}
          style={{
            minWidth: "var(--radix-dropdown-menu-trigger-width)",
            width: "var(--radix-dropdown-menu-trigger-width)",
          }}
        >
          <div
            className="max-h-[min(16rem,var(--radix-dropdown-menu-content-available-height))] overflow-y-auto overscroll-contain"
            onTouchMoveCapture={(event) => event.stopPropagation()}
            onWheelCapture={(event) => event.stopPropagation()}
          >
            <DropdownMenuRadioGroup
              onValueChange={(nextValue) => {
                onValueChange(nextValue);
                setOpen(false);
              }}
              value={value}
            >
              {options.map((option) => (
                <DropdownMenuRadioItem
                  disabled={option.disabled}
                  key={option.value}
                  value={option.value}
                >
                  <span className="truncate">{option.label}</span>
                </DropdownMenuRadioItem>
              ))}
            </DropdownMenuRadioGroup>
          </div>
        </DropdownMenuContent>
      </DropdownMenu>
    </div>
  );
}
