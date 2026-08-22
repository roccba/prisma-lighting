import { useEffect, useRef, useState } from "react";

// The top application menu bar. Data-driven: the window passes a tree of menus and this
// component renders the bar, the drop-downs and nested fly-outs, shows keyboard-shortcut
// hints, and greys out disabled entries. It owns only which top-level menu is open; the
// actions live with the window (so the keyboard shortcuts and the menu run the same code).

export type MenuNode =
  | {
      kind: "action";
      label: string;
      shortcut?: string;
      onSelect?: () => void;
      disabled?: boolean;
      checked?: boolean;
    }
  | { kind: "submenu"; label: string; children: MenuNode[]; disabled?: boolean }
  | { kind: "divider" };

export interface MenuDef {
  label: string;
  children: MenuNode[];
}

export function MenuBar({ menus }: { menus: MenuDef[] }) {
  const [open, setOpen] = useState<string | null>(null);
  const barRef = useRef<HTMLDivElement>(null);

  // Close on outside click or Escape (a menu bar should never trap the pointer).
  useEffect(() => {
    if (!open) return;
    const onDown = (e: MouseEvent) => {
      if (barRef.current && !barRef.current.contains(e.target as Node)) setOpen(null);
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setOpen(null);
    };
    document.addEventListener("mousedown", onDown);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onDown);
      document.removeEventListener("keydown", onKey);
    };
  }, [open]);

  const runAction = (node: Extract<MenuNode, { kind: "action" }>) => {
    setOpen(null);
    node.onSelect?.();
  };

  return (
    <div className="pr-menubar" ref={barRef}>
      {menus.map((menu) => (
        <div
          key={menu.label}
          className={`mi${open === menu.label ? " is-open" : ""}`}
          onClick={() => setOpen((cur) => (cur === menu.label ? null : menu.label))}
          // Once a menu is open, hovering another top item switches to it (standard bar UX).
          onMouseEnter={() => setOpen((cur) => (cur !== null ? menu.label : cur))}
        >
          {menu.label}
          {open === menu.label && (
            <div className="pr-menu-drop" onClick={(e) => e.stopPropagation()}>
              {menu.children.map((node, i) => (
                <MenuNodeView key={i} node={node} onAction={runAction} />
              ))}
            </div>
          )}
        </div>
      ))}
    </div>
  );
}

function MenuNodeView({
  node,
  onAction,
}: {
  node: MenuNode;
  onAction: (n: Extract<MenuNode, { kind: "action" }>) => void;
}) {
  if (node.kind === "divider") return <div className="pr-menu-div" />;

  if (node.kind === "submenu") {
    return (
      <div className={`pr-menu-item pr-menu-parent${node.disabled ? " is-disabled" : ""}`}>
        <span className="pr-menu-label">{node.label}</span>
        <span className="pr-menu-caret">▸</span>
        {!node.disabled && (
          <div className="pr-menu-flyout">
            {node.children.map((child, i) => (
              <MenuNodeView key={i} node={child} onAction={onAction} />
            ))}
          </div>
        )}
      </div>
    );
  }

  return (
    <div
      className={`pr-menu-item${node.disabled ? " is-disabled" : ""}${node.checked ? " is-checked" : ""}`}
      onClick={node.disabled ? undefined : () => onAction(node)}
    >
      <span className="pr-menu-label">
        {node.checked ? "✓ " : ""}
        {node.label}
      </span>
      {node.shortcut && <span className="pr-menu-kbd">{node.shortcut}</span>}
    </div>
  );
}
