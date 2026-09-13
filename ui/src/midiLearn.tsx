import { createContext, useCallback, useContext, useEffect, useState, type ReactNode } from "react";
import { listen } from "@tauri-apps/api/event";
import {
  controlLabel,
  midiLearnArm,
  midiLearnCancel,
  midiStatus,
  midiUnmap,
  sameTarget,
  type MapTarget,
  type MidiMapping,
} from "./api";

// MIDI Learn (M9-SPEC §7). A window-wide *learn mode* (Ctrl+M, like the consoles and VJ
// tools operators already know): while it is on, every learnable control shows an overlay.
// Click one to ARM it — it tints soft orange and pulses ("touch something now") — then move
// a fader or hit a pad on the hardware and the engine binds that control to it. Right-click
// a mapped control to remove its mapping. Esc leaves learn mode.
//
// The capture itself happens in the engine (the next MIDI message after arming); the UI
// only arms a target and listens for `midi-learned`. Targets are built by each control from
// what it is (a zone slot, a global path) — never from a hardcoded list here.

interface LearnCtx {
  active: boolean;
  setActive: (on: boolean) => void;
  armed: MapTarget | null;
  /** The target that was just bound, for a short confirmation flash. */
  justLearned: MapTarget | null;
  mappings: MidiMapping[];
  arm: (target: MapTarget) => void;
  unmap: (target: MapTarget) => void;
}

const Ctx = createContext<LearnCtx | null>(null);

export function MidiLearnProvider({ children }: { children: ReactNode }) {
  const [active, setActiveState] = useState(false);
  const [armed, setArmed] = useState<MapTarget | null>(null);
  const [justLearned, setJustLearned] = useState<MapTarget | null>(null);
  const [mappings, setMappings] = useState<MidiMapping[]>([]);

  const refresh = useCallback(() => {
    midiStatus()
      .then((s) => setMappings(s.mappings))
      .catch(() => {});
  }, []);

  const setActive = useCallback(
    (on: boolean) => {
      setActiveState(on);
      if (on) refresh();
      else {
        setArmed(null);
        midiLearnCancel().catch(() => {});
      }
    },
    [refresh],
  );

  useEffect(() => {
    const un = listen<MidiMapping>("midi-learned", (e) => {
      setArmed(null);
      setJustLearned(e.payload.target);
      window.setTimeout(() => setJustLearned(null), 900);
      refresh();
    });
    return () => {
      un.then((f) => f()).catch(() => {});
    };
  }, [refresh]);

  useEffect(() => {
    if (!active) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setActive(false);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [active, setActive]);

  const arm = useCallback((target: MapTarget) => {
    setArmed(target);
    midiLearnArm(target).catch(() => setArmed(null));
  }, []);

  const unmap = useCallback((target: MapTarget) => {
    midiUnmap(target)
      .then(setMappings)
      .catch(() => {});
  }, []);

  return (
    <Ctx.Provider value={{ active, setActive, armed, justLearned, mappings, arm, unmap }}>
      {children}
    </Ctx.Provider>
  );
}

export function useMidiLearn(): LearnCtx {
  const ctx = useContext(Ctx);
  if (!ctx) throw new Error("useMidiLearn outside MidiLearnProvider");
  return ctx;
}

interface LearnTargetProps {
  target: MapTarget;
  /** What the control is, for the tooltip ("Dimmer — Zona 1"). */
  name: string;
  children: ReactNode;
  className?: string;
}

/** Wraps a learnable control. Outside learn mode it renders only its children. */
export function LearnTarget({ target, name, children, className }: LearnTargetProps) {
  const { active, armed, justLearned, mappings, arm, unmap } = useMidiLearn();
  const mapping = mappings.find((m) => sameTarget(m.target, target));
  const isArmed = armed !== null && sameTarget(armed, target);
  const isFlash = justLearned !== null && sameTarget(justLearned, target);

  const title = isArmed
    ? `${name}: mové un control del hardware…`
    : mapping
      ? `${name}: ${controlLabel(mapping.control)} — clic para reasignar, clic derecho para quitar`
      : `${name}: clic y mové un control del hardware`;

  return (
    <div className={`pr-learn-host${className ? ` ${className}` : ""}`}>
      {children}
      {active && (
        <div
          className={`pr-learn-overlay${mapping ? " is-mapped" : ""}${isArmed ? " is-armed" : ""}${isFlash ? " is-learned" : ""}`}
          title={title}
          onPointerDown={(e) => {
            e.stopPropagation();
            e.preventDefault();
            if (e.button === 0) arm(target);
          }}
          onContextMenu={(e) => {
            e.preventDefault();
            e.stopPropagation();
            if (mapping) unmap(target);
          }}
        >
          {mapping && !isArmed && <span className="pr-learn-badge">{controlLabel(mapping.control)}</span>}
        </div>
      )}
    </div>
  );
}
