import { fireEvent, render, screen } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import type { ReactNode } from "react";

import type { ItemCardData } from "../library/ItemCard";
import { DetailPanel } from "./DetailPanel";

const commands = vi.hoisted(() => ({
  getItemExt: vi.fn(async (id: string) => ({
    item: {
      id,
      kind: "image",
      status: "ready",
      capturedAt: 1_700_000_000_000,
      sourceApp: "chrome.exe",
      sourceTitle: "发票页面",
      sourceUrl: null as string | null,
      width: 1280,
      height: 800,
      charCount: null as number | null,
      note: "",
      thumbUrl: "visrecall://thumb/1",
    },
    extras: {
      summary: "一张橙色发票截图，金额 1280 元" as string | null,
      summarySource: "vlm",
      ocrBoxes: [
        { text: "发票号码 00329181", x: 0.1, y: 0.2, w: 0.3, h: 0.05 },
        { text: "金额 1280.00", x: 0.1, y: 0.3, w: 0.2, h: 0.05 },
      ],
      bodyText: null as string | null,
      originalUrl: "visrecall://original/abc123" as string | null,
      tags: [] as { id: string; name: string; auto: boolean; hue: string | null; itemCount: number }[],
    },
  })),
  noteSet: vi.fn(async () => undefined),
  copyItemOriginal: vi.fn(async () => undefined),
  itemDelete: vi.fn(async () => undefined),
  tagAdd: vi.fn(async () => ({ id: "t", name: "x", auto: false, hue: null, itemCount: 1 })),
  tagDetach: vi.fn(async () => undefined),
  tagsList: vi.fn(async () => []),
}));

vi.mock("../../lib/bindings", () => ({ commands }));

const item: ItemCardData = {
  id: "abc123",
  kind: "image",
  status: "ready",
  capturedAt: 1_700_000_000_000,
  sourceApp: "chrome.exe",
  sourceTitle: "发票页面",
  thumbUrl: "visrecall://thumb/abc123",
};

function renderPanel(node: ReactNode) {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(<QueryClientProvider client={client}>{node}</QueryClientProvider>);
}

test("renders the real summary with its source badge and enables copy", async () => {
  renderPanel(<DetailPanel item={item} onClose={() => {}} />);
  expect(await screen.findByText("一张橙色发票截图，金额 1280 元")).toBeInTheDocument();
  expect(screen.getByText("VLM")).toBeInTheDocument();
  expect(screen.getByRole("button", { name: "复制" })).toBeEnabled();
});

test("opens the overlay with OCR highlight boxes", async () => {
  renderPanel(<DetailPanel item={item} onClose={() => {}} />);
  const button = await screen.findByText(/在图中查看 2 处文字/);
  fireEvent.click(button);
  const overlay = await screen.findByRole("dialog", { name: "查看原图" });
  expect(overlay.querySelectorAll(".ocr-box")).toHaveLength(2);
});

test("opening the original from the thumbnail does not highlight OCR text", async () => {
  renderPanel(<DetailPanel item={item} onClose={() => {}} />);
  // 等详情（含两个 OCR 框）到位，再走「查看原图」入口。
  await screen.findByText(/在图中查看 2 处文字/);

  fireEvent.click(screen.getByLabelText("查看原图"));

  const overlay = await screen.findByRole("dialog", { name: "查看原图" });
  expect(overlay.querySelectorAll(".ocr-box")).toHaveLength(0);
});

test("no boxes means no highlight action and no overlay boxes", async () => {
  const original = commands.getItemExt.getMockImplementation();
  commands.getItemExt.mockImplementationOnce(async (id: string) => {
    const base = await original!(id);
    return {
      ...base,
      extras: { ...base.extras, ocrBoxes: [], summary: null, summarySource: "none" },
    };
  });
  renderPanel(<DetailPanel item={item} onClose={() => {}} />);
  expect(await screen.findByText(/待解析/)).toBeInTheDocument();
  expect(screen.queryByText(/在图中查看/)).not.toBeInTheDocument();
  fireEvent.click(screen.getByLabelText("查看原图"));
  const overlay = await screen.findByRole("dialog", { name: "查看原图" });
  expect(overlay.querySelectorAll(".ocr-box")).toHaveLength(0);
});
