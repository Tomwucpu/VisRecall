import { act, renderHook } from "@testing-library/react";
import { useSelection } from "./useSelection";

const ids = ["a", "b", "c", "d"];

test("entering and exiting the mode always resets the selection", () => {
  const { result } = renderHook(() => useSelection(ids));

  expect(result.current.selectionMode).toBe(false);
  act(() => result.current.enter());
  expect(result.current.selectionMode).toBe(true);

  act(() => result.current.toggle("a"));
  expect(result.current.count).toBe(1);

  act(() => result.current.exit());
  expect(result.current.selectionMode).toBe(false);
  expect(result.current.count).toBe(0);
});

test("shift-click selects the inclusive range from the anchor", () => {
  const { result } = renderHook(() => useSelection(ids));

  act(() => result.current.enter());
  act(() => result.current.toggle("a"));
  act(() => result.current.toggle("c", true));

  expect([...result.current.selectedIds].sort()).toEqual(["a", "b", "c"]);

  // 再按 Shift 从新锚点往回选：仍只增加，不取消已有项。
  act(() => result.current.toggle("b", true));
  expect([...result.current.selectedIds].sort()).toEqual(["a", "b", "c"]);
});

test("select all loaded and clear keep the order of the list", () => {
  const { result } = renderHook(() => useSelection(ids));

  act(() => result.current.enter());
  act(() => result.current.selectAllLoaded());
  expect(result.current.ids).toEqual(ids);

  act(() => result.current.clear());
  expect(result.current.count).toBe(0);
});

test("remove drops deleted ids without touching the rest", () => {
  const { result } = renderHook(() => useSelection(ids));

  act(() => result.current.enter());
  act(() => result.current.selectMany(["a", "b", "c"]));
  act(() => result.current.remove(["b"]));

  expect([...result.current.selectedIds].sort()).toEqual(["a", "c"]);
});

test("toggle removes an already selected id", () => {
  const { result } = renderHook(() => useSelection(ids));

  act(() => result.current.enter());
  act(() => result.current.toggle("b"));
  act(() => result.current.toggle("b"));
  expect(result.current.count).toBe(0);
});
