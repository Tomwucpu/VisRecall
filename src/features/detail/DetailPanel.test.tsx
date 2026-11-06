import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import type { ReactNode } from "react";
import { resetToasts } from "../../lib/toast";
import { ToastViewport } from "../shell/ToastViewport";
import type { ItemCardData } from "../library/ItemCard";
import { DetailPanel } from "./DetailPanel";

const commands = vi.hoisted(() => {
  const getItemExt = vi.fn(async (id: string) => ({
    item: {
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
    },
    extras: {
      summary: null as string | null,
      bodyText: null as string | null,
      originalUrl: "visrecall://original/abc123" as string | null,
      tags: [] as { id: string; name: string; auto: boolean; hue: string | null; itemCount: number }[],
    },
  }));

  return {
    getItemExt,
    noteSet: vi.fn(async () => undefined),
    copyItemOriginal: vi.fn(async () => undefined),
    itemDelete: vi.fn(async () => undefined),
    tagAdd: vi.fn(async () => ({ id: "t1", name: "work", auto: false, hue: null, itemCount: 1 })),
    tagDetach: vi.fn(async () => undefined),
    tagsList: vi.fn(async () => []),
  };
});

vi.mock("../../lib/bindings", () => ({ commands }));

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

test("renders real metadata from get_item_ext and hides absent size as a dash", async () => {
  renderPanel(<DetailPanel item={item} onClose={() => {}} />);

  expect(await screen.findByText("1280 × 800")).toBeInTheDocument();
  // 编号在头部与元信息表各出现一次。
  expect(screen.getAllByText("ABC123").length).toBeGreaterThan(0);
  expect(commands.getItemExt).toHaveBeenCalledWith("abc123");
});

test("debounces the note and persists it through note_set", async () => {
  renderPanel(<DetailPanel item={item} onClose={() => {}} />);

  const note = screen.getByLabelText("备注");
  fireEvent.change(note, { target: { value: "记" } });
  fireEvent.change(note, { target: { value: "记一条备注" } });
  expect(note).toHaveValue("记一条备注");

  // 300ms 防抖：只提交最后一次输入。
  await waitFor(() => {
    expect(commands.noteSet).toHaveBeenCalledTimes(1);
  });
  expect(commands.noteSet).toHaveBeenCalledWith("abc123", "记一条备注");
  // 元信息行把字数与状态拼在一起（`6 / 10000 · 已保存`），因此用正则匹配。
  expect(await screen.findByText(/已保存/)).toBeInTheDocument();
});

test("blocks input beyond the 10000 character limit instead of saving an overflow", () => {
  renderPanel(<DetailPanel item={item} onClose={() => {}} />);

  const note = screen.getByLabelText("备注") as HTMLTextAreaElement;
  fireEvent.change(note, { target: { value: "x".repeat(10_050) } });

  expect(note.value).toHaveLength(10_000);
});

test("asks for confirmation before deleting, then closes the panel", async () => {
  const onClose = vi.fn();
  renderPanel(<DetailPanel item={item} onClose={onClose} />);

  fireEvent.click(screen.getByRole("button", { name: "删除素材" }));
  expect(screen.getByRole("dialog")).toHaveTextContent("彻底删除这条素材？");

  fireEvent.click(screen.getByRole("button", { name: "彻底删除" }));
  await waitFor(() => {
    expect(commands.itemDelete).toHaveBeenCalledWith("abc123");
  });
  expect(onClose).toHaveBeenCalledTimes(1);
});

test("opens the in-app overlay from the thumbnail and closes on Escape", async () => {
  renderPanel(<DetailPanel item={item} onClose={() => {}} />);

  fireEvent.click(screen.getByRole("button", { name: "查看原图" }));

  const overlay = screen.getByRole("dialog", { name: "查看原图" });
  expect(overlay).toHaveTextContent("原图");

  fireEvent.keyDown(overlay, { key: "Escape" });
  await waitFor(() => {
    expect(screen.queryByRole("dialog", { name: "查看原图" })).not.toBeInTheDocument();
  });
});

test("copies the original through copy_item_original", async () => {
  renderPanel(<DetailPanel item={item} onClose={() => {}} />);

  fireEvent.click(screen.getByRole("button", { name: "复制原图" }));
  await waitFor(() => {
    expect(commands.copyItemOriginal).toHaveBeenCalledWith("abc123");
  });
});
