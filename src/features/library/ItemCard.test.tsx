import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { toOriginalSrc, toThumbSrc } from "../../lib/format";
import { resetToasts } from "../../lib/toast";
import { ItemCard, type ItemCardData } from "./ItemCard";
import { toItemCardData } from "./useItems";

const commands = vi.hoisted(() => ({
  copyItemOriginal: vi.fn(async () => undefined),
}));

vi.mock("../../lib/bindings", () => ({ commands }));

afterEach(() => {
  resetToasts();
  vi.clearAllMocks();
});

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

test("hides the status badge for a normal item but keeps it for abnormal ones", () => {
  const { rerender } = render(<ItemCard item={{ ...base, status: "ready" }} />);
  expect(screen.queryByText("正常")).not.toBeInTheDocument();

  rerender(<ItemCard item={{ ...base, status: "failed" }} />);
  expect(screen.getByText("失败")).toBeInTheDocument();
});

test("copies the original from the card without opening the detail", async () => {
  const onOpen = vi.fn();
  render(<ItemCard item={{ ...base, status: "ready" }} onOpen={onOpen} />);

  fireEvent.click(screen.getByRole("button", { name: "复制原图" }));

  await waitFor(() => {
    expect(commands.copyItemOriginal).toHaveBeenCalledWith("1");
  });
  expect(onOpen).not.toHaveBeenCalled();
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

test("maps the original protocol url to a loadable address", () => {
  expect(toOriginalSrc("visrecall://original/abc123")).toBe(
    "http://visrecall.localhost/original/abc123",
  );
  expect(toOriginalSrc(null)).toBeNull();
  expect(toOriginalSrc("https://example.test/a.png")).toBe("https://example.test/a.png");
  // 已经转换过的地址不能被二次转换（详情页与卡片共用同一条数据）。
  expect(toOriginalSrc("http://visrecall.localhost/original/abc123")).toBe(
    "http://visrecall.localhost/original/abc123",
  );
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
