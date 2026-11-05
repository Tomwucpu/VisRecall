import { fireEvent, render, screen } from "@testing-library/react";
import { toThumbSrc } from "../../lib/format";
import { ItemCard, type ItemCardData } from "./ItemCard";
import { toItemCardData } from "./useItems";

// jsdom 里没有 Tauri 运行时：`convertFileSrc` 返回与应用内一致的协议地址。
vi.mock("@tauri-apps/api/core", () => ({
  convertFileSrc: () => "http://visrecall.localhost/thumb",
}));

const base: ItemCardData = {
  id: "1",
  kind: "image",
  status: "parsing",
  capturedAt: 1_700_000_000_000,
  sourceApp: "chrome.exe",
  sourceTitle: "Test",
  thumbUrl: null,
};

test("shows parsing status", () => {
  render(<ItemCard item={base} />);
  expect(screen.getByText("解析中")).toBeInTheDocument();
});

test("shows ready thumbnail", () => {
  render(<ItemCard item={{ ...base, id: "2", status: "ready", thumbUrl: "visrecall://thumb/2" }} />);
  expect(screen.getByRole("img")).toHaveAttribute("src", "visrecall://thumb/2");
});

test("shows status label for pending and failed", () => {
  const { rerender } = render(<ItemCard item={{ ...base, status: "pending" }} />);
  expect(screen.getByText("待解析")).toBeInTheDocument();

  rerender(<ItemCard item={{ ...base, status: "failed" }} />);
  expect(screen.getByText("失败")).toBeInTheDocument();
});

test("shows source metadata and placeholder without thumb", () => {
  render(<ItemCard item={base} />);
  expect(screen.getByText("chrome.exe")).toBeInTheDocument();
  expect(screen.getByText("Test")).toBeInTheDocument();
  expect(screen.queryByRole("img")).not.toBeInTheDocument();
});

test("maps dto thumb url to a custom-protocol address", () => {
  const dto = {
    id: "abc123",
    kind: "image",
    status: "ready",
    capturedAt: 1_700_000_000_000,
    sourceApp: null,
    sourceTitle: null,
    sourceUrl: null,
    width: 640,
    height: 400,
    charCount: null,
    note: "",
    thumbUrl: "visrecall://thumb/abc123",
  };

  expect(toItemCardData(dto).thumbUrl).toBe("http://visrecall.localhost/thumb/abc123");
  expect(toThumbSrc("visrecall://thumb/abc")).toBe("http://visrecall.localhost/thumb/abc");
  expect(toThumbSrc(null)).toBeNull();
  expect(toThumbSrc("http://example.test/a.png")).toBe("http://example.test/a.png");
});

test("falls back to placeholder when the thumbnail fails to load, and retries on src change", () => {
  const { rerender } = render(
    <ItemCard item={{ ...base, id: "3", status: "ready", thumbUrl: "visrecall://thumb/3" }} />,
  );
  fireEvent.error(screen.getByRole("img"));

  expect(screen.queryByRole("img")).not.toBeInTheDocument();
  // 占位文案与类型徽标都用「图片」，因此这里允许两处。
  expect(screen.getAllByText("图片")).toHaveLength(2);

  // 换一张图应重置失败状态，重新尝试加载。
  rerender(<ItemCard item={{ ...base, id: "3", status: "ready", thumbUrl: "visrecall://thumb/4" }} />);
  expect(screen.getByRole("img")).toHaveAttribute("src", "visrecall://thumb/4");
});

test("shows a fallback time instead of throwing on an invalid capturedAt", () => {
  render(<ItemCard item={{ ...base, capturedAt: Number.NaN }} />);
  expect(screen.getByText("未知时间")).toBeInTheDocument();
});

test("falls back to Chinese labels for unknown status and kind", () => {
  render(<ItemCard item={{ ...base, status: "teleported", kind: "video" }} />);
  expect(screen.getByText("未知状态")).toBeInTheDocument();
  // 未知类型同时出现在类型徽标和缩略图占位上。
  expect(screen.getAllByText("未知类型")).toHaveLength(2);
  expect(screen.queryByText("teleported")).not.toBeInTheDocument();
  expect(screen.queryByText("video")).not.toBeInTheDocument();
});
