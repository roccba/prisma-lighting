import { createContext, useCallback, useContext, useEffect, useState, type ReactNode } from "react";
import { listen } from "@tauri-apps/api/event";
import {
  controlLabel,
  midiConfigure,
  midiLearnArm,
  midiSetTakeover,
  midiLearnCancel,
  midiStatus,
  midiUnmap,
  sameTarget,
  type ControlMode,
  type LedAnimation,
  type LedRate,
  type LedStyle,
  type Bank,
  type MapTarget,
  type MidiMapping,
  type Takeover,
} from "./api";

// MIDI Learn (M9-SPEC §7). A window-wide *learn mode* (Ctrl+M, like the consoles and VJ
// tools operators already know): while it is on, every learnable control shows an overlay.
// Click one to ARM it — it tints soft orange and pulses ("touch something now") — then move
// a fader or hit a pad on the hardware and the engine binds that control to it. Right-click
// a mapped control to configure it: its button style (latch / trigger / toggle / momentary),
// its LED colours and motion, or remove the mapping. Esc leaves learn mode.
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
  /** Open the configuration menu for a mapped target at a screen position. */
  openConfig: (target: MapTarget, name: string, x: number, y: number) => void;
  /** Where the controller's grid window sits: grid cells and scenes are learned
   *  relative to it (the pad under the operator's finger). */
  bank: Bank;
  /** The connected device's grid window (tracks × rows), if it has one and banks. */
  gridWindow: [number, number] | null;
}

interface ConfigAt {
  target: MapTarget;
  name: string;
  x: number;
  y: number;
}

const Ctx = createContext<LearnCtx | null>(null);

export function MidiLearnProvider({ children }: { children: ReactNode }) {
  const [active, setActiveState] = useState(false);
  const [armed, setArmed] = useState<MapTarget | null>(null);
  const [justLearned, setJustLearned] = useState<MapTarget | null>(null);
  const [mappings, setMappings] = useState<MidiMapping[]>([]);
  const [palette, setPalette] = useState<string[]>([]);
  const [config, setConfig] = useState<ConfigAt | null>(null);
  const [bank, setBank] = useState<Bank>({ track: 0, row: 0 });
  const [gridWindow, setGridWindow] = useState<[number, number] | null>(null);

  const refresh = useCallback(() => {
    midiStatus()
      .then((s) => {
        setMappings(s.mappings);
        setPalette(s.palette);
        setBank(s.bank);
        setGridWindow(s.connected ? s.grid_window : null);
      })
      .catch(() => {});
  }, []);

  // The bank buttons move the window; a device seeding its defaults adds mappings.
  useEffect(() => {
    refresh();
    const subs = [
      listen<Bank>("midi-bank", (e) => setBank(e.payload)),
      listen("midi-seeded", refresh),
      listen("midi-mappings-changed", refresh),
      listen("midi-connected", refresh),
    ];
    return () => {
      subs.forEach((u) => u.then((f) => f()).catch(() => {}));
    };
  }, [refresh]);

  const setActive = useCallback(
    (on: boolean) => {
      setActiveState(on);
      if (on) refresh();
      else {
        setArmed(null);
        setConfig(null);
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
      if (e.key !== "Escape") return;
      if (config) setConfig(null);
      else setActive(false);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [active, setActive, config]);

  const arm = useCallback((target: MapTarget) => {
    setArmed(target);
    midiLearnArm(target).catch(() => setArmed(null));
  }, []);

  const unmap = useCallback((target: MapTarget) => {
    midiUnmap(target)
      .then(setMappings)
      .catch(() => {});
  }, []);

  const openConfig = useCallback((target: MapTarget, name: string, x: number, y: number) => {
    setConfig({ target, name, x, y });
  }, []);

  const configMapping = config ? mappings.find((m) => sameTarget(m.target, config.target)) : undefined;

  return (
    <Ctx.Provider value={{ active, setActive, armed, justLearned, mappings, arm, unmap, openConfig, bank, gridWindow }}>
      {children}
      {config && configMapping && (
        <MappingMenu
          at={config}
          mapping={configMapping}
          palette={palette}
          onChange={(mode, led) =>
            midiConfigure(config.target, mode, led)
              .then(setMappings)
              .catch(() => {})
          }
          onTakeover={(takeover) =>
            midiSetTakeover(config.target, takeover)
              .then(setMappings)
              .catch(() => {})
          }
          onUnmap={() => {
            unmap(config.target);
            setConfig(null);
          }}
          onClose={() => setConfig(null)}
        />
      )}
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
  const { active, armed, justLearned, mappings, arm, openConfig } = useMidiLearn();
  const mapping = mappings.find((m) => sameTarget(m.target, target));
  const isArmed = armed !== null && sameTarget(armed, target);
  const isFlash = justLearned !== null && sameTarget(justLearned, target);

  const title = isArmed
    ? `${name}: mové un control del hardware…`
    : mapping
      ? `${name}: ${controlLabel(mapping.control)} — clic para reasignar, clic derecho para configurar`
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
            if (mapping) openConfig(target, name, e.clientX, e.clientY);
          }}
        >
          {mapping && !isArmed && <span className="pr-learn-badge">{controlLabel(mapping.control)}</span>}
        </div>
      )}
    </div>
  );
}

// --- The right-click configuration menu --------------------------------------------------

const MODE_LABEL: Record<ControlMode, string> = {
  absolute: "Absoluto",
  latch: "Latch",
  trigger: "Trigger",
  toggle: "Toggle",
  momentary: "Momentary",
};

const MODE_HINT: Record<ControlMode, string> = {
  absolute: "La posición del control es el valor.",
  latch: "Tocás y queda como el clip del track (igual que lanzarlo en pantalla).",
  trigger: "Un golpe: reinicia el efecto desde el principio, sin importar cuánto mantengas.",
  toggle: "Un toque lo activa; el siguiente vuelve a lo que había antes.",
  momentary: "Activo mientras mantenés; al soltar vuelve a lo que había antes.",
};

const RATES: { value: LedRate; label: string }[] = [
  { value: "n24", label: "1/24" },
  { value: "n16", label: "1/16" },
  { value: "n8", label: "1/8" },
  { value: "n4", label: "1/4" },
  { value: "n2", label: "1/2" },
];

/** Which button styles make sense for a target: all four on a grid clip, on/off styles on
 *  a switch (solo, mute, blackout, strobe), none on a fader or a one-shot action (GO, stop,
 *  a scene, select). */
function modesFor(target: MapTarget): ControlMode[] {
  const onOff: ControlMode[] = ["toggle", "momentary"];
  if ("path" in target) {
    const switches = ["/master/blackout", "/master/strobe/on"];
    return switches.includes(target.path) ? onOff : [];
  }
  if ("zone_slot" in target) {
    const sub = target.zone_slot.sub;
    return sub === "solo" || sub === "blackout" || sub === "strobe_on" ? onOff : [];
  }
  if ("track_slot" in target) {
    const sub = target.track_slot.sub;
    if (typeof sub === "object") return ["latch", "trigger", "toggle", "momentary"];
    return sub === "mute" || sub === "solo" ? onOff : [];
  }
  return [];
}

/** Whether a target is a grid clip (a pad with a four-state LED). */
function isClip(target: MapTarget): boolean {
  return "track_slot" in target && typeof target.track_slot.sub === "object";
}

function animKind(a: LedAnimation | undefined): "solid" | "pulse" | "blink" {
  if (!a || a === "solid") return "solid";
  return "pulse" in a ? "pulse" : "blink";
}

function animRate(a: LedAnimation | undefined): LedRate {
  if (!a || a === "solid") return "n8";
  return "pulse" in a ? a.pulse : a.blink;
}

interface MappingMenuProps {
  at: ConfigAt;
  mapping: MidiMapping;
  palette: string[];
  onChange: (mode: ControlMode, led: LedStyle | null) => void;
  onTakeover: (takeover: Takeover) => void;
  onUnmap: () => void;
  onClose: () => void;
}

const TAKEOVER_HINT: Record<Takeover, string> = {
  pickup:
    "El control no mueve el valor hasta alcanzarlo: al conectar el controlador o si la pantalla lo cambió, no salta.",
  jump: "La posición del control se aplica siempre, de inmediato (puede saltar).",
};

function MappingMenu({ at, mapping, palette, onChange, onTakeover, onUnmap, onClose }: MappingMenuProps) {
  const modes = modesFor(at.target);
  // A fader / knob driving a continuous value (no button styles apply to it).
  const isContinuous = modes.length === 0 && "cc" in mapping.control.kind && mapping.mode === "absolute";
  const takeover: Takeover = mapping.takeover ?? "pickup";
  const isCell = isClip(at.target);
  const isNote = "note" in mapping.control.kind;
  const led: LedStyle = mapping.led ?? {};
  const [slot, setSlot] = useState<"on" | "idle">("on");

  // Keep the menu on screen and close it on an outside click.
  const [pos, setPos] = useState({ x: at.x, y: at.y });
  const ref = useCallback(
    (el: HTMLDivElement | null) => {
      if (!el) return;
      const r = el.getBoundingClientRect();
      const x = Math.max(8, Math.min(at.x, window.innerWidth - r.width - 8));
      const y = Math.max(8, Math.min(at.y, window.innerHeight - r.height - 8));
      if (x !== pos.x || y !== pos.y) setPos({ x, y });
    },
    [at.x, at.y, pos.x, pos.y],
  );
  useEffect(() => {
    const onDown = (e: PointerEvent) => {
      if (!(e.target as HTMLElement).closest(".pr-midimenu")) onClose();
    };
    window.addEventListener("pointerdown", onDown, true);
    return () => window.removeEventListener("pointerdown", onDown, true);
  }, [onClose]);

  const setLed = (next: LedStyle) => {
    const empty = next.on === undefined && next.idle === undefined && animKind(next.animation) === "solid";
    onChange(mapping.mode, empty ? null : next);
  };
  const setAnim = (kind: "solid" | "pulse" | "blink", rate: LedRate) => {
    const animation: LedAnimation =
      kind === "solid" ? "solid" : kind === "pulse" ? { pulse: rate } : { blink: rate };
    setLed({ ...led, animation });
  };
  const chosen = slot === "on" ? led.on : led.idle;

  return (
    <div
      ref={ref}
      className="pr-midimenu"
      style={{ left: pos.x, top: pos.y }}
      onContextMenu={(e) => e.preventDefault()}
    >
      <div className="pr-midimenu-title">
        {at.name}
        <span className="pr-midimenu-sub">{controlLabel(mapping.control)}</span>
      </div>

      {modes.length > 0 && (
        <div className="pr-midimenu-section">
          <div className="pr-midimenu-label">Estilo de pulsador</div>
          <div className="pr-midimenu-seg">
            {modes.map((m) => (
              <button
                key={m}
                className={`pr-btn${mapping.mode === m ? " is-active" : ""}`}
                onClick={() => onChange(m, mapping.led ?? null)}
              >
                {MODE_LABEL[m]}
              </button>
            ))}
          </div>
          <div className="pr-midimenu-hint">{MODE_HINT[mapping.mode]}</div>
        </div>
      )}

      {isContinuous && (
        <div className="pr-midimenu-section">
          <div className="pr-midimenu-label">Toma de control</div>
          <div className="pr-midimenu-seg">
            {(["pickup", "jump"] as const).map((t) => (
              <button
                key={t}
                className={`pr-btn${takeover === t ? " is-active" : ""}`}
                onClick={() => onTakeover(t)}
              >
                {t === "pickup" ? "Pickup" : "Salto directo"}
              </button>
            ))}
          </div>
          <div className="pr-midimenu-hint">{TAKEOVER_HINT[takeover]}</div>
        </div>
      )}

      {isNote && palette.length > 0 && (
        <div className="pr-midimenu-section">
          <div className="pr-midimenu-label">Color del LED</div>
          {isCell && (
            <div className="pr-midimenu-seg">
              <button className={`pr-btn${slot === "on" ? " is-active" : ""}`} onClick={() => setSlot("on")}>
                En vivo
              </button>
              <button className={`pr-btn${slot === "idle" ? " is-active" : ""}`} onClick={() => setSlot("idle")}>
                Cargada
              </button>
            </div>
          )}
          <div className="pr-swatches">
            <button
              className={`pr-swatch pr-swatch-auto${chosen === undefined ? " is-selected" : ""}`}
              title={isCell ? "Automático: color de la zona del track" : "Automático"}
              onClick={() => setLed({ ...led, [slot]: undefined })}
            >
              A
            </button>
            {palette.slice(1).map((hex, i) => (
              <button
                key={i + 1}
                className={`pr-swatch${chosen === i + 1 ? " is-selected" : ""}`}
                style={{ background: hex }}
                title={`Color ${i + 1}`}
                onClick={() => setLed({ ...led, [slot]: i + 1 })}
              />
            ))}
          </div>

          <div className="pr-midimenu-label">Animación (en vivo)</div>
          <div className="pr-midimenu-seg">
            {(["solid", "pulse", "blink"] as const).map((k) => (
              <button
                key={k}
                className={`pr-btn${animKind(led.animation) === k ? " is-active" : ""}`}
                onClick={() => setAnim(k, animRate(led.animation))}
              >
                {k === "solid" ? "Fijo" : k === "pulse" ? "Pulso" : "Parpadeo"}
              </button>
            ))}
          </div>
          {animKind(led.animation) !== "solid" && (
            <div className="pr-midimenu-seg">
              {RATES.map((r) => (
                <button
                  key={r.value}
                  className={`pr-btn${animRate(led.animation) === r.value ? " is-active" : ""}`}
                  onClick={() => setAnim(animKind(led.animation), r.value)}
                >
                  {r.label}
                </button>
              ))}
            </div>
          )}
        </div>
      )}

      <div className="pr-midimenu-actions">
        <button className="pr-btn" onClick={onUnmap}>
          Quitar mapeo
        </button>
        <button className="pr-btn is-primary" onClick={onClose}>
          Listo
        </button>
      </div>
    </div>
  );
}
