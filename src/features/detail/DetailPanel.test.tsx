import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import type { ReactNode } from "react";
import { resetToasts } from "../../lib/toast";
import { ToastViewport } from "../shell/ToastViewport";
import type { ItemCardData } from "../library/ItemCard";
import { DetailPanel } from "./DetailPanel";

const getItem = vi.hoisted(() =>
  vi.fn(async (id: string) => ({
    id,
    kind: "image",
    status: "ready",
    capturedAt: 1_700_000_000_000,
    sourceApp: "chrome.exe",
    sourceTitle: "Test",
    sourceUrl: null as string | null,
    width: 1280,
    height: 800,
    charCount: null as number | null,
    note: "",
    thumbUrl: "visrecall://thumb/1",
  })),
);

vi.mock("../../lib/bindings", () => ({
  commands: {
    getItem,
    getAppStatus: vi.fn(),
    listItems: vi.fn(),
    openLibraryFolder: vi.fn(),
    getSettings: vi.fn(),
  },
}));

const item: ItemCardData = {
  id: "abc123",
  kind: "image",
  status: "ready",
  capturedAt: 1_700_000_000_000,
  sourceApp: "chrome.exe",
  sourceTitle: "Test",
  thumbUrl: "visrecall://thumb/abc123",
};

function renderPanel(node: ReactNode) {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(
    <QueryClientProvider client={client}>
      {node}
      <ToastViewport />
    </QueryClientProvider>,
  );
}

afterEach(() => {
  resetToasts();
  vi.clearAllMocks();
});

test("shows the AI summary placeholder and keeps copy disabled", async () => {
  renderPanel(<DetailPanel item={item} onClose={() => {}} />);

  expect(await screen.findByText("待解析（阶段 03 提供）")).toBeInTheDocument();
  expect(screen.getByRole("button", { name: "复制" })).toBeDisabled();
  expect(screen.getByRole("button", { name: "复制原图" })).toBeEnabled();
});

test("renders real metadata from get_item and hides absent size as a dash", async () => {
  renderPanel(<DetailPanel item={item} onClose={() => {}} />);

  expect(await screen.findByText("1280 × 800")).toBeInTheDocument();
  // 编号在头部与元信息表各出现一次。
  expect(screen.getAllByText("ABC123").length).toBeGreaterThan(0);
  expect(getItem).toHaveBeenCalledWith("abc123");
});

test("debounces the note and reports that persistence is not wired yet", async () => {
  renderPanel(<DetailPanel item={item} onClose={() => {}} />);

  const note = screen.getByLabelText("备注");
  fireEvent.change(note, { target: { value: "记一条备注" } });
  expect(note).toHaveValue("记一条备注");

  expect(await screen.findByText(/待阶段 02 提供 · 仅本地草稿/)).toBeInTheDocument();
});

test("blocks input beyond the 10000 character limit instead of saving an overflow", () => {
  renderPanel(<DetailPanel item={item} onClose={() => {}} />);

  const note = screen.getByLabelText("备注") as HTMLTextAreaElement;
  fireEvent.change(note, { target: { value: "x".repeat(10_050) } });

  expect(note.value).toHaveLength(10_000);
});

test("asks for confirmation before deleting, then surfaces the stub message", async () => {
  renderPanel(<DetailPanel item={item} onClose={() => {}} />);

  fireEvent.click(screen.getByRole("button", { name: "删除素材" }));
  expect(screen.getByRole("dialog")).toHaveTextContent("彻底删除这条素材？");

  fireEvent.click(screen.getByRole("button", { name: "彻底删除" }));
  expect(await screen.findByText("删除将在阶段 02 提供")).toBeInTheDocument();
});

test("opens the in-app overlay from the thumbnail and closes on Escape", async () => {
  renderPanel(<DetailPanel item={item} onClose={() => {}} />);

  fireEvent.click(screen.getByRole("button", { name: "查看原图" }));

  const overlay = screen.getByRole("dialog", { name: "查看原图" });
  expect(overlay).toHaveTextContent("缩略图 · 原图待阶段 02 提供");

  fireEvent.keyDown(overlay, { key: "Escape" });
  await waitFor(() => {
    expect(screen.queryByRole("dialog", { name: "查看原图" })).not.toBeInTheDocument();
  });
});

test("surfaces the copy stub instead of pretending to copy", async () => {
  renderPanel(<DetailPanel item={item} onClose={() => {}} />);

  fireEvent.click(screen.getByRole("button", { name: "复制原图" }));
  expect(await screen.findByText("复制原图将在阶段 02 提供")).toBeInTheDocument();
});
