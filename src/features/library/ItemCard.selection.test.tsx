import { fireEvent, render, screen } from "@testing-library/react";
import { ItemCard, type ItemCardData } from "./ItemCard";

const base: ItemCardData = {
  id: "1",
  kind: "image",
  status: "ready",
  capturedAt: 1_700_000_000_000,
  sourceApp: "chrome.exe",
  sourceTitle: "来源标题",
  title: null,
  thumbUrl: null,
};

test("prefers the user title and falls back to the source title", () => {
  const { rerender } = render(<ItemCard item={base} />);
  expect(screen.getByText("来源标题")).toBeInTheDocument();

  rerender(<ItemCard item={{ ...base, title: "我的标题" }} />);
  expect(screen.getByText("我的标题")).toBeInTheDocument();
  expect(screen.queryByText("来源标题")).not.toBeInTheDocument();

  // 清空标题（null）后回到来源标题，而不是显示空白。
  rerender(<ItemCard item={{ ...base, title: "   " }} />);
  expect(screen.getByText("来源标题")).toBeInTheDocument();
});

test("selection mode toggles instead of opening and keeps the checkbox visible", () => {
  const onOpen = vi.fn();
  const onToggleSelect = vi.fn();
  const { rerender } = render(
    <ItemCard item={base} onOpen={onOpen} onToggleSelect={onToggleSelect} selectionMode />,
  );

  const card = screen.getByRole("checkbox");
  expect(card).toHaveAttribute("aria-checked", "false");
  expect(document.querySelector(".card-check")).not.toBeNull();
  expect(screen.queryByRole("button", { name: "复制原图" })).not.toBeInTheDocument();

  fireEvent.click(card);
  expect(onToggleSelect).toHaveBeenCalledWith(base, false);
  expect(onOpen).not.toHaveBeenCalled();

  rerender(
    <ItemCard
      item={base}
      onOpen={onOpen}
      onToggleSelect={onToggleSelect}
      selectionMode
      selected
    />,
  );
  expect(screen.getByRole("checkbox")).toHaveAttribute("aria-checked", "true");
  expect(document.querySelector(".card-check")).toHaveClass("is-checked");
});

test("shift-click reports the modifier for range selection", () => {
  const onToggleSelect = vi.fn();
  render(<ItemCard item={base} selectionMode onToggleSelect={onToggleSelect} />);

  fireEvent.click(screen.getByRole("checkbox"), { shiftKey: true });
  expect(onToggleSelect).toHaveBeenCalledWith(base, true);
});

test("card no longer exposes a rename entry outside selection mode", () => {
  render(<ItemCard item={base} onOpen={vi.fn()} />);
  expect(screen.queryByRole("button", { name: "重命名" })).not.toBeInTheDocument();
});
