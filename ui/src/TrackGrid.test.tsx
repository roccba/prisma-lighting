// The grid's cell gestures and states (LIVE-MODEL-SPEC §7, §10.3 item 2), on the demo show:
// a click selects and never plays; Shift+click arms; a click on an armed cell disarms; a
// double click launches (arms in Modo armar); every cell reads Vacía / Cargada / En vivo /
// Armada, the same words everywhere.
import { describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { TrackGrid } from "./TrackGrid";
import { MidiLearnProvider } from "./midiLearn";
import { cellState } from "./cellParts";
import type { ZoneInfo } from "./api";
import fixture from "../e2e/fixtures/demo.json";

type Fx = { main_grid: { tracks: { id: string; clips: { armed: boolean; active: boolean }[] }[] } };
const live = () => (window as unknown as { __fixture: Fx }).__fixture;
const calls = (cmd: string) => window.__calls.filter((c) => c.cmd === cmd).map((c) => c.args);

async function renderGrid(props: { armMode?: boolean } = {}) {
  const onSelect = vi.fn();
  render(
    <MidiLearnProvider>
      <TrackGrid
        zones={fixture.main_zones as unknown as ZoneInfo[]}
        selectedTrack={null}
        selectedCells={{}}
        onSelect={onSelect}
        armMode={props.armMode ?? false}
        rev={0}
        onChanged={() => {}}
        density="normal"
      />
    </MidiLearnProvider>,
  );
  await waitFor(() => expect(document.querySelectorAll(".pr-cell.is-assigned").length).toBeGreaterThan(10));
  return { onSelect };
}

/** Track 0 ("Techo · Fondo"), clip `i`. */
const cell = (i: number) =>
  document.querySelectorAll<HTMLElement>(`.pr-cell[data-track="${live().main_grid.tracks[0].id}"]`)[i];

describe("cell gestures", () => {
  it("a click selects and never plays or arms", async () => {
    const { onSelect } = await renderGrid();
    fireEvent.pointerDown(cell(1), { button: 0 });
    expect(onSelect).toHaveBeenCalled();
    expect(calls("main_arm_cell")).toEqual([]);
    expect(calls("main_trigger_cell")).toEqual([]);
  });

  it("Shift+click arms the clip", async () => {
    await renderGrid();
    fireEvent.pointerDown(cell(1), { button: 0, shiftKey: true });
    await waitFor(() => expect(calls("main_arm_cell")).toEqual([{ track: live().main_grid.tracks[0].id, index: 1 }]));
    expect(calls("main_trigger_cell")).toEqual([]);
  });

  it("a click on an armed clip disarms it (with or without Shift)", async () => {
    live().main_grid.tracks[0].clips[1].armed = true;
    try {
      await renderGrid();
      expect(cell(1).className).toContain("is-armed");
      fireEvent.pointerDown(cell(1), { button: 0 });
      await waitFor(() => expect(calls("main_arm_cell").length).toBe(1));
      fireEvent.pointerDown(cell(1), { button: 0, shiftKey: true });
      await waitFor(() => expect(calls("main_arm_cell").length).toBe(2));
    } finally {
      live().main_grid.tracks[0].clips[1].armed = false;
    }
  });

  it("a double click launches", async () => {
    await renderGrid();
    fireEvent.doubleClick(cell(1));
    await waitFor(() => expect(calls("main_trigger_cell")).toEqual([{ track: live().main_grid.tracks[0].id, index: 1 }]));
  });

  it("in Modo armar a double click arms instead", async () => {
    await renderGrid({ armMode: true });
    fireEvent.doubleClick(cell(1));
    await waitFor(() => expect(calls("main_arm_cell").length).toBe(1));
    expect(calls("main_trigger_cell")).toEqual([]);
  });

  it("an empty clip has nothing to play", async () => {
    await renderGrid();
    fireEvent.doubleClick(cell(3)); // row 4 of Techo · Fondo is empty (the strobe row)
    fireEvent.pointerDown(cell(3), { button: 0, shiftKey: true });
    expect(calls("main_trigger_cell")).toEqual([]);
    expect(calls("main_arm_cell")).toEqual([]);
  });
});

describe("cell states", () => {
  it("names the four states", () => {
    expect(cellState({ effect_id: null, active: false, armed: false })).toBe("Vacía");
    expect(cellState({ effect_id: "plasma", active: false, armed: false })).toBe("Cargada");
    expect(cellState({ effect_id: "plasma", active: true, armed: false })).toBe("En vivo");
    expect(cellState({ effect_id: "plasma", active: false, armed: true })).toBe("Armada");
    expect(cellState({ effect_id: "plasma", active: true, armed: true }), "running wins").toBe("En vivo");
  });

  it("the grid shows each cell's state in its tooltip and classes", async () => {
    live().main_grid.tracks[0].clips[1].armed = true;
    try {
      await renderGrid();
      expect(cell(0).title).toContain("En vivo"); // "Tranquilo" runs when the show opens
      expect(cell(0).className).toContain("is-active");
      expect(cell(1).title).toContain("Armada");
      expect(cell(2).title).toContain("Cargada");
      expect(cell(3).title).toContain("Vacía");
      expect(cell(3).className).toContain("is-empty");
      expect(screen.getAllByText("Plasma lento").length).toBeGreaterThan(0);
    } finally {
      live().main_grid.tracks[0].clips[1].armed = false;
    }
  });
});
