import { describe, expect, it } from "vitest";
import { moveMembers } from "./GroupsPanel";

describe("moveMembers", () => {
  const m = ["a", "b", "c", "d", "e"];
  it("moves a picked block one place, keeping its order", () => {
    expect(moveMembers(m, new Set(["b", "c"]), 1)).toEqual(["a", "d", "b", "c", "e"]);
    expect(moveMembers(m, new Set(["b", "c"]), -1)).toEqual(["b", "c", "a", "d", "e"]);
  });
  it("moves scattered picks each one place", () => {
    expect(moveMembers(m, new Set(["b", "d"]), -1)).toEqual(["b", "a", "d", "c", "e"]);
  });
  it("a block at the edge stays", () => {
    expect(moveMembers(m, new Set(["a", "c"]), -1)).toEqual(m);
    expect(moveMembers(m, new Set(["e"]), 1)).toEqual(m);
    expect(moveMembers(m, new Set(["z"]), 1)).toEqual(m);
  });
});
