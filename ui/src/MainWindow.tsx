import { useEffect, useState } from "react";
import { mainZones, type ZoneInfo } from "./api";
import { EffectParams } from "./EffectParams";
import { GlobalBar } from "./GlobalBar";
import { LargePreview } from "./LargePreview";
import { PreviewStreamProvider } from "./previewStream";
import { SceneGrid } from "./SceneGrid";
import { ZoneRow } from "./ZoneRow";

// The M8 main operation window (M8-SPEC §3). Step 3 builds the FIXED layout skeleton
// and the populated zone row; the scene grid, large preview, parameter panel and
// global operation bar are placeholders whose regions are laid out here and filled in
// steps 4–6. Each region is a self-contained panel that does not assume its neighbours
// (§7), so the later dashboard milestone can make them dockable without a rewrite.
//
// The window owns the ONE piece of cross-region state step 3 needs: the *selected*
// zone (a UI state — which zone the grid/preview/params will show). Selecting never
// changes output (selected ≠ active, §3.2); only the zone cards' faders / solo / mute
// do that.

const MENU = ["Archivo", "Vista", "Preferencias", "Herramientas", "Ayuda"];

export default function MainWindow() {
  const [zones, setZones] = useState<ZoneInfo[]>([]);
  const [selected, setSelected] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  // Bumped whenever the selected zone's active cell changes (fire / assign / clear), so
  // the effect parameter panel re-fetches the now-active effect and its values. The
  // large preview follows the stream on its own and needs no signal.
  const [gridRev, setGridRev] = useState(0);

  useEffect(() => {
    mainZones()
      .then((zs) => {
        setZones(zs);
        setSelected((cur) => cur ?? (zs.length > 0 ? zs[0].id : null));
      })
      .catch((e) => setError(String(e)));
  }, []);

  const selectedZone = zones.find((z) => z.id === selected) ?? null;

  return (
    <PreviewStreamProvider>
      <div className="pr-mainwin">
        <div className="pr-menubar">
          {MENU.map((m) => (
            <span className="mi" key={m}>
              {m}
            </span>
          ))}
        </div>

        <div className="pr-mainwin-body">
          <section className="pr-region pr-region-zonerow">
            <div className="pr-region-title">Zonas</div>
            <ZoneRow zones={zones} selected={selected} onSelect={setSelected} />
          </section>

          <div className="pr-mainwin-mid">
            <section className="pr-region pr-region-grid">
              <div className="pr-region-title">
                Grilla{selectedZone ? ` — ${selectedZone.name}` : ""}
              </div>
              {selectedZone ? (
                <SceneGrid
                  key={selectedZone.id}
                  zoneId={selectedZone.id}
                  onActiveChange={() => setGridRev((r) => r + 1)}
                />
              ) : (
                <div className="pr-placeholder-body">Seleccioná una zona</div>
              )}
            </section>
            <section className="pr-region pr-region-preview">
              <div className="pr-region-title">
                Preview{selectedZone ? ` — ${selectedZone.name}` : ""}
              </div>
              {selectedZone ? (
                <LargePreview key={selectedZone.id} zoneId={selectedZone.id} />
              ) : (
                <div className="pr-placeholder-body">Seleccioná una zona</div>
              )}
            </section>
          </div>

          <section className="pr-region pr-region-params">
            <div className="pr-region-title">
              Parámetros{selectedZone ? ` — ${selectedZone.name}` : ""}
            </div>
            {selectedZone ? (
              <EffectParams zoneId={selectedZone.id} rev={gridRev} />
            ) : (
              <div className="pr-placeholder-body">Seleccioná una zona</div>
            )}
          </section>

          <section className="pr-region pr-region-global">
            <GlobalBar />
          </section>
        </div>

        {error && <div className="pr-mainwin-error">Error: {error}</div>}
      </div>
    </PreviewStreamProvider>
  );
}
