import { render, screen } from "@testing-library/react";
import { toThumbSrc } from "../../lib/format";
import { ItemCard, type ItemCardData } from "./ItemCard";
import { toItemCardData } from "./useItems";

// jsdom 里没有 Tauri 运行时：把 `convertFileSrc` 换成可预期的等价物。
vi.mock("@tauri-apps/api/core", () => ({
  convertFileSrc: (path: string, protocol = "asset") => `${protocol}://${path}`,
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

  expect(toItemCardData(dto).thumbUrl).toBe("visrecall://thumb/abc123");
  expect(toThumbSrc(null)).toBeNull();
  expect(toThumbSrc("http://example.test/a.png")).toBe("http://example.test/a.png");
});
