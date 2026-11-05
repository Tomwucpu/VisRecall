import { render, screen } from "@testing-library/react";
import { EmptyState } from "./EmptyState";

test("guides the user to capture the first item", () => {
  render(<EmptyState />);
  expect(screen.getByText("还没有素材")).toBeInTheDocument();
  expect(screen.getByText("按 Ctrl+Shift+S 或点击悬浮球开始捕获")).toBeInTheDocument();
});
