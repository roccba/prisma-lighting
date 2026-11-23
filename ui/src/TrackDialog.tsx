import { useEffect, useState } from "react";
import {
  controlLabel,
  renameTrack,
  sameTarget,
  setTrackBehaviour,
  setTrackBlend,
  setTrackDestination,
  setTrackOpacity,
  type BlendMode,
  type Easing,
  type LaunchMode,
  type MapTarget,
  type TrackInfo,
  type TrackSub,
  type TrackTransition,
  type ZoneInfo,
} from "./api";
import { Check } from "./Check";
import { Dropdown } from "./Dropdown";
import { useMidiLearn } from "./midiLearn";
import { NumberField } from "./NumberField";

// The track settings dialog (LIVE-MODEL-SPEC §7, phase 4): everything that builds a track
// — name, destination zones, blend, opacity, transition, launch mode and its MIDI
// controls. Every field applies live, as it does on the grid, except the destination:
// that rebuilds the show (a recompile), so it is staged and applied with "Aplicar
// destino". The ⋯ popover on the header keeps quick access to blend and transition.

export const EASINGS: { value: Easing; label: string }[] = [
  { value: "linear", label: "Lineal" },
  { value: "ease_in", label: "Entrada suave" },
  { value: "ease_out", label: "Salida suave" },
  { value: "ease_in_out", label: "Suave (S)" },
];

/** The blend modes, in console order (the popover and the dialog use the same list). */
export const BLENDS: { value: BlendMode; label: string }[] = [
  { value: "Normal", label: "Normal" },
  { value: "Add", label: "Add" },
  { value: "Subtract", label: "Subtract" },
  { value: "Multiply", label: "Multiply" },
  { value: "Screen", label: "Screen" },
  { value: "Max", label: "Max" },
  { value: "Min", label: "Min" },
  { value: "Mask", label: "Mask" },
  { value: "LumaKey", label: "Luma key" },
];

/** A blend mode's label (the header, the popover and the dialog use the same words). */
export function blendLabel(mode: BlendMode): string {
  return BLENDS.find((b) => b.value === mode)?.label ?? mode;
}

/** Blend mode + the luma key's threshold and softness when it is the key. */
export function BlendFields({
  track,
  onBlend,
}: {
  track: TrackInfo;
  onBlend: (mode: BlendMode, threshold: number, softness: number) => void;
}) {
  return (
    <>
      <div className="pr-tsettings-row">
        <span className="pr-tsettings-label">Mezcla</span>
        <Dropdown
          value={track.blend_mode}
          options={BLENDS}
          onChange={(v) => onBlend(v as BlendMode, track.key_threshold, track.key_softness)}
          minWidth={140}
          aria-label="Modo de mezcla"
        />
      </div>
      {track.blend_mode === "LumaKey" && (
        <>
          <div className="pr-tsettings-row">
            <span className="pr-tsettings-label">Umbral</span>
            <NumberField
              value={Math.round(track.key_threshold * 100)}
              min={0}
              max={100}
              step={1}
              width={60}
              onChange={(v) => onBlend("LumaKey", v / 100, track.key_softness)}
              aria-label="Umbral del key"
            />
            <span className="pr-tsettings-unit">%</span>
          </div>
          <div className="pr-tsettings-row">
            <span className="pr-tsettings-label">Suavidad</span>
            <NumberField
              value={Math.round(track.key_softness * 100)}
              min={0}
              max={100}
              step={1}
              width={60}
              onChange={(v) => onBlend("LumaKey", track.key_threshold, v / 100)}
              aria-label="Suavidad del key"
            />
            <span className="pr-tsettings-unit">%</span>
          </div>
        </>
      )}
    </>
  );
}

/** Transition time, curve and time base. */
export function TransitionFields({
  transition: t,
  onChange,
}: {
  transition: TrackTransition;
  onChange: (t: TrackTransition) => void;
}) {
  return (
    <>
      <div className="pr-tsettings-row">
        <span className="pr-tsettings-label">Transición</span>
        <NumberField
          value={t.time}
          min={0}
          max={600}
          step={t.base === "beats" ? 1 : 0.1}
          width={60}
          onChange={(time) => onChange({ ...t, time })}
          aria-label="Tiempo de transición"
        />
        <span className="pr-tsettings-unit">{t.base === "beats" ? "beats" : "s"}</span>
      </div>
      <div className="pr-tsettings-row">
        <span className="pr-tsettings-label">Curva</span>
        <Dropdown
          value={t.easing}
          options={EASINGS}
          onChange={(v) => onChange({ ...t, easing: v as Easing })}
          minWidth={140}
          aria-label="Curva de la transición"
        />
      </div>
      <div className="pr-tsettings-row">
        <span className="pr-tsettings-label" />
        <Check checked={t.base === "beats"} onChange={(on) => onChange({ ...t, base: on ? "beats" : "seconds" })}>
          En beats
        </Check>
      </div>
    </>
  );
}

/** The track's MIDI controls, in the footer's order. */
const MIDI_CONTROLS: { sub: TrackSub; label: string }[] = [
  { sub: "opacity", label: "Opacidad" },
  { sub: "mute", label: "Mute" },
  { sub: "solo", label: "Solo" },
  { sub: "stop", label: "Stop" },
  { sub: "select", label: "Seleccionar" },
];

export function TrackDialog({
  track,
  slot,
  zones,
  onPatch,
  onClose,
}: {
  track: TrackInfo;
  /** The track's position in the grid (its MIDI targets are relative to the bank window). */
  slot: number;
  zones: ZoneInfo[];
  /** Mirror a live change into the grid at once (the engine confirms on the next fetch). */
  onPatch: (change: Partial<TrackInfo>) => void;
  onClose: () => void;
}) {
  const [name, setName] = useState(track.name);
  const [dest, setDest] = useState<string[]>(track.zones);
  const [applying, setApplying] = useState(false);
  const [message, setMessage] = useState<string | null>(null);
  const { bank, mappings, armed, arm, unmap } = useMidiLearn();

  // The grid re-fetches after a recompile: follow the stored destination.
  const stored = track.zones.join();
  useEffect(() => setDest(track.zones), [stored]); // eslint-disable-line react-hooks/exhaustive-deps

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  const commitName = () => {
    const n = name.trim();
    if (!n || n === track.name) {
      setName(track.name);
      return;
    }
    onPatch({ name: n });
    renameTrack(track.id, n).catch(() => {});
  };

  // In patch order, like the engine stores it.
  const ordered = zones.filter((z) => dest.includes(z.id)).map((z) => z.id);
  const destChanged = ordered.join() !== track.zones.join();
  const toggleZone = (id: string, on: boolean) =>
    setDest((d) => (on ? [...d.filter((z) => z !== id), id] : d.filter((z) => z !== id)));
  const applyDest = () => {
    if (!destChanged || ordered.length === 0) return;
    setApplying(true);
    setMessage(null);
    setTrackDestination(track.id, ordered)
      .catch((e) => setMessage(String(e)))
      .finally(() => setApplying(false));
  };

  const behaviour = (transition: TrackTransition, launch: LaunchMode) => {
    onPatch({ transition, launch });
    setTrackBehaviour(track.id, transition, launch).catch(() => {});
  };
  const blend = (mode: BlendMode, threshold: number, softness: number) => {
    onPatch({ blend_mode: mode, key_threshold: threshold, key_softness: softness });
    setTrackBlend(track.id, mode, threshold, softness).catch(() => {});
  };
  const opacity = (v: number) => {
    onPatch({ opacity: v });
    setTrackOpacity(track.id, v).catch(() => {});
  };

  const target = (sub: TrackSub): MapTarget | null =>
    slot >= bank.track ? { track_slot: { slot: slot - bank.track, sub } } : null;

  return (
    <div className="pr-picker-backdrop" onPointerDown={onClose}>
      <div
        className="pr-picker pr-trackdlg"
        role="dialog"
        aria-label={`Ajustes del track ${track.name}`}
        onPointerDown={(e) => e.stopPropagation()}
      >
        <div className="pr-picker-head">Ajustes del track</div>
        <div className="pr-picker-body pr-trackdlg-body">
          <div className="pr-tsettings-row">
            <span className="pr-tsettings-label">Nombre</span>
            <input
              className="pr-devmgr-input pr-trackdlg-name"
              value={name}
              maxLength={64}
              spellCheck={false}
              aria-label="Nombre del track"
              onChange={(e) => setName(e.target.value)}
              onBlur={commitName}
              onKeyDown={(e) => {
                e.stopPropagation();
                if (e.key === "Enter") (e.target as HTMLInputElement).blur();
                if (e.key === "Escape") {
                  setName(track.name);
                  onClose();
                }
              }}
            />
          </div>

          <section className="pr-trackdlg-section">
            <div className="pr-trackdlg-title">Destino</div>
            <div className="pr-trackdlg-zones">
              {zones.map((z) => {
                const on = dest.includes(z.id);
                const last = on && dest.length === 1;
                return (
                  <div key={z.id} className="pr-trackdlg-zone" title={last ? "Un track necesita al menos una zona" : undefined}>
                    <Check checked={on} disabled={last} onChange={(v) => toggleZone(z.id, v)}>
                      <span className="pr-trackdlg-swatch" style={{ background: `rgb(${z.color.join(",")})` }} />
                      {z.name}
                    </Check>
                  </div>
                );
              })}
            </div>
            <div className="pr-trackdlg-apply">
              <span className="pr-tsettings-hint">
                Las zonas comparten los parámetros del efecto; cada una conserva su VEL. Aplicar recompila el show:
                las animaciones arrancan de nuevo y se desarma lo armado.
              </span>
              <button
                type="button"
                className="pr-btn"
                disabled={!destChanged || ordered.length === 0 || applying}
                onClick={applyDest}
              >
                {applying ? "Aplicando…" : "Aplicar destino"}
              </button>
            </div>
            {message && <div className="pr-artnet-warn">{message}</div>}
          </section>

          <section className="pr-trackdlg-section">
            <div className="pr-trackdlg-title">Composición</div>
            <BlendFields track={track} onBlend={blend} />
            <div className="pr-tsettings-row">
              <span className="pr-tsettings-label">Opacidad</span>
              <NumberField
                value={Math.round(track.opacity * 255)}
                min={0}
                max={255}
                step={1}
                width={60}
                onChange={(v) => opacity(v / 255)}
                aria-label="Opacidad del track"
              />
            </div>
          </section>

          <section className="pr-trackdlg-section">
            <div className="pr-trackdlg-title">Lanzamiento</div>
            <TransitionFields transition={track.transition} onChange={(t) => behaviour(t, track.launch)} />
            <div className="pr-tsettings-row">
              <span className="pr-tsettings-label">Al tocar</span>
              <Dropdown
                value={track.launch}
                options={[
                  { value: "direct", label: "Lanzar directo" },
                  { value: "arm", label: "Armar (espera GO)" },
                ]}
                onChange={(v) => behaviour(track.transition, v as LaunchMode)}
                minWidth={140}
                aria-label="Modo de lanzamiento"
              />
            </div>
            <div className="pr-tsettings-hint">0 = corte. Una celda con fade propio lo usa en lugar de este.</div>
          </section>

          <section className="pr-trackdlg-section">
            <div className="pr-trackdlg-title">MIDI</div>
            {MIDI_CONTROLS.map(({ sub, label }) => {
              const t = target(sub);
              const mapping = t ? mappings.find((m) => sameTarget(m.target, t)) : undefined;
              const isArmed = t !== null && armed !== null && sameTarget(armed, t);
              return (
                <div key={label} className="pr-tsettings-row">
                  <span className="pr-tsettings-label">{label}</span>
                  <span className="pr-trackdlg-ctl">
                    {t === null ? "fuera de la ventana" : isArmed ? "mové un control…" : mapping ? controlLabel(mapping.control) : "—"}
                  </span>
                  <button
                    type="button"
                    className={`pr-btn${isArmed ? " is-active" : ""}`}
                    disabled={t === null}
                    onClick={() => t && arm(t)}
                    title="Clic y mové un control del hardware"
                  >
                    Aprender
                  </button>
                  {mapping && t && (
                    <button type="button" className="pr-btn" onClick={() => unmap(t)}>
                      Quitar
                    </button>
                  )}
                </div>
              );
            })}
            <div className="pr-tsettings-hint">
              Los controles se asignan a la posición del track en la ventana del controlador, no al track en sí.
            </div>
          </section>
        </div>
        <div className="pr-devmgr-foot">
          <button className="pr-btn" type="button" onClick={onClose}>
            Cerrar
          </button>
        </div>
      </div>
    </div>
  );
}
