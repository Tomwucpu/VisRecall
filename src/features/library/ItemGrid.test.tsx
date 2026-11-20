import { fireEvent, render, screen } from "@testing-library/react";
import { ItemGrid } from "./ItemGrid";
import type { ItemCardData } from "./ItemCard";

const item: ItemCardData = {
  id: "1",
  kind: "image",
  status: "ready",
  capturedAt: 1_700_000_000_000,
  sourceApp: "chrome.exe",
  sourceTitle: "Test",
  thumbUrl: null,
};

const baseProps = {
  isPending: false,
  isError: false,
  errorMessage: null,
  hasNextPage: false,
  isFetchingNextPage: false,
  onLoadMore: () => {},
  onRetry: () => {},
};

test("keeps the grid and shows a retry bar when a refetch fails with data", () => {
  const onRetry = vi.fn();
  render(
    <ItemGrid
      {...baseProps}
      items={[item]}
      isError
      errorMessage="db: locked"
      onRetry={onRetry}
    />,
  );

  expect(screen.getByText("Test")).toBeInTheDocument();
  expect(screen.getByText(/刷新失败/)).toBeInTheDocument();

  fireEvent.click(screen.getByRole("button", { name: "重试" }));
  expect(onRetry).toHaveBeenCalledTimes(1);
});

test("shows the full-page error card when nothing has loaded yet", () => {
  render(<ItemGrid {...baseProps} items={[]} isError />);

  expect(screen.getByText("无法读取素材")).toBeInTheDocument();
  expect(screen.queryByRole("list")).not.toBeInTheDocument();
});

test("keeps loaded cards mounted while the list is pending again", () => {
  render(<ItemGrid {...baseProps} items={[item]} isPending />);

  // 刷新期间保留网格：卸载会闪白、丢滚动位置，并让缩略图全部重新解码。
  expect(screen.getByText("Test")).toBeInTheDocument();
  expect(screen.queryByText("正在加载素材…")).not.toBeInTheDocument();
});
