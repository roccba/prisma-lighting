import { useState } from "react";

// Prompt for a new zone's name. Names must be unique (the engine enforces this too);
// validating here gives immediate feedback and blocks the create until the name is
// valid, so two zones can never end up sharing a name and overwriting each other.

interface Props {
  /** Names already in use — EXCLUDING the one being renamed, so a rename to itself is fine. */
  existingNames: string[];
  onCreate: (name: string) => void;
  onClose: () => void;
  /** Dialog title (default "New zone"). */
  title?: string;
  /** Initial value (default empty) — set to the current name when renaming. */
  initial?: string;
  /** Confirm-button label (default "Create"). */
  submitLabel?: string;
}

export function NewZoneDialog({
  existingNames,
  onCreate,
  onClose,
  title = "New zone",
  initial = "",
  submitLabel = "Create",
}: Props) {
  const [name, setName] = useState(initial);
  const taken = new Set(existingNames.map((n) => n.trim().toLowerCase()));
  const trimmed = name.trim();
  const duplicate = trimmed !== "" && taken.has(trimmed.toLowerCase());
  const valid = trimmed !== "" && !duplicate;

  const submit = () => {
    if (valid) onCreate(trimmed);
  };

  return (
    <div className="modal-backdrop" onClick={onClose}>
      <div className="modal" onClick={(e) => e.stopPropagation()}>
        <div className="modal-head">{title}</div>
        <div className="modal-body">
          <label className="field">
            <span>Name</span>
            <input
              value={name}
              autoFocus
              onChange={(e) => setName(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter") submit();
                else if (e.key === "Escape") onClose();
              }}
            />
          </label>
          {duplicate && <div className="modal-error">A zone named «{trimmed}» already exists.</div>}
        </div>
        <div className="modal-foot">
          <div className="spacer" />
          <button className="btn" onClick={onClose}>Cancel</button>
          <button className="btn primary" onClick={submit} disabled={!valid}>{submitLabel}</button>
        </div>
      </div>
    </div>
  );
}
