import { fireEvent, render, screen } from "@testing-library/react";
import { EmptyState } from "./EmptyState";

test("guides the user to capture the first item", () => {
  render(<EmptyState onAdd={() => {}} />);
  expect(screen.getByText("还没有素材")).toBeInTheDocument();
  expect(screen.getByText("按 Ctrl+Shift+S 或点击悬浮球开始捕获")).toBeInTheDocument();
});

test("opens the shared import flow from the empty library", () => {
  const onAdd = vi.fn();
  render(<EmptyState onAdd={onAdd} />);
  fireEvent.click(screen.getByRole("button", { name: "＋ 添加素材" }));
  expect(onAdd).toHaveBeenCalledTimes(1);
});
