import { useEffect, useRef } from "react";
import { Menu, MenuContent, MenuItem, MenuTrigger } from "./ui/Menu";

// Small floating menu anchored at viewport (x, y) — the right-click menu for
// sidebar folders, chat conversations and note rows.
//
// The public shape (x/y/onClose plus <ContextMenuItem> children) is unchanged;
// underneath it is now the shared `Menu` primitive (#114), which brings the
// arrow-key roving, typeahead and real collision detection this copy never had.
// Radix anchors content to a trigger rather than to raw coordinates, so the
// trigger here is a zero-size element pinned at the pointer.
export function ContextMenu({
  x,
  y,
  onClose,
  children,
}: {
  x: number;
  y: number;
  onClose: () => void;
  children: React.ReactNode;
}) {
  // Radix moves focus into the menu on open and hands it back to the trigger on
  // close — but our trigger is virtual and unmounts with the menu, so focus
  // would land on <body>. Remember what had focus when the menu opened (the
  // right-clicked row) and put it back there instead.
  const restoreRef = useRef<Element | null>(null);
  useEffect(() => {
    restoreRef.current = document.activeElement;
  }, []);

  // Close on scroll. Radix keeps content glued to its anchor, but ours is
  // pinned to raw viewport coordinates — scrolling the list underneath would
  // leave the menu hovering over a different row than the one it acts on.
  useEffect(() => {
    const onScroll = () => onClose();
    document.addEventListener("scroll", onScroll, true);
    return () => document.removeEventListener("scroll", onScroll, true);
  }, [onClose]);

  return (
    <Menu
      open
      onOpenChange={(open) => {
        if (!open) onClose();
      }}
    >
      <MenuTrigger asChild>
        <span aria-hidden style={{ position: "fixed", left: x, top: y, width: 0, height: 0 }} />
      </MenuTrigger>
      <MenuContent
        sideOffset={0}
        maxHeight={320}
        className="bg-[var(--color-surface)] rounded-md"
        onCloseAutoFocus={(e) => {
          e.preventDefault();
          const node = restoreRef.current;
          if (node instanceof HTMLElement && document.body.contains(node)) node.focus();
        }}
      >
        {children}
      </MenuContent>
    </Menu>
  );
}

export function ContextMenuItem({
  onClick,
  children,
  danger,
}: {
  onClick: () => void;
  children: React.ReactNode;
  danger?: boolean;
}) {
  return (
    <MenuItem danger={danger} onSelect={onClick} className="px-3 rounded-sm">
      {children}
    </MenuItem>
  );
}
