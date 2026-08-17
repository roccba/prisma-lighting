import { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { mainZones, openEditor, type ZoneInfo } from "./api";
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
  // Which menu is dropped down (null = none). Minimal for M8: only Preferencias has an
  // action (open the patch editor, §6). A full menu system is later polish.
  const [menuOpen, setMenuOpen] = useState<string | null>(null);
  // Bumped whenever the selected zone's active cell changes (fire / assign / clear), so
  // the effect parameter panel re-fetches the now-active effect and its values. The
  // large preview follows the stream on its own and needs no signal.
  const [gridRev, setGridRev] = useState(0);

  const refreshZones = () =>
    mainZones()
      .then((zs) => {
        setZones(zs);
        // Keep the selection if it still exists; otherwise fall back to the first zone.
        setSelected((cur) => (cur && zs.some((z) => z.id === cur) ? cur : (zs[0]?.id ?? null)));
      })
      .catch((e) => setError(String(e)));

  useEffect(() => {
    refreshZones();
  }, []);

  // The patch editor (secondary window) applies its edits to the running show on close
  // and emits `layout-changed` (§7). The zone set / names / fixtures may have changed,
  // so re-fetch the row and refresh the grid/params.
  useEffect(() => {
    const un = listen("layout-changed", () => {
      refreshZones();
      setGridRev((r) => r + 1);
    });
    return () => {
      un.then((f) => f()).catch(() => {});
    };
  }, []);

  const selectedZone = zones.find((z) => z.id === selected) ?? null;

  return (
    <PreviewStreamProvider>
      <div className="pr-mainwin">
        <div className="pr-menubar" onPointerLeave={() => setMenuOpen(null)}>
          {MENU.map((m) => (
            <span
              className={`mi${menuOpen === m ? " is-open" : ""}`}
              key={m}
              onClick={() => setMenuOpen((cur) => (cur === m ? null : m))}
            >
              {m}
              {m === "Preferencias" && (
                <div className="submenu" onClick={(e) => e.stopPropagation()}>
                  <div
                    className="subitem"
                    onClick={() => {
                      setMenuOpen(null);
                      openEditor().catch(() => {});
                    }}
                  >
                    Editor de Patch
                    <span className="kbd">F3</span>
                  </div>
                </div>
              )}
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
