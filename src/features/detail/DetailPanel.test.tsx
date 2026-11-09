import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
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
    itemReparse: vi.fn(async () => undefined),
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

test("reparse triggers the command and reports it with a toast", async () => {
  renderPanel(<DetailPanel item={item} onClose={() => {}} />);

  fireEvent.click(screen.getByRole("button", { name: "重新解析" }));
  await waitFor(() => {
    expect(commands.itemReparse).toHaveBeenCalledWith("abc123");
  });
  expect(await screen.findByText("已重新开始解析")).toBeInTheDocument();
});

test("reparse reports a missing item and closes the panel", async () => {
  const onClose = vi.fn();
  commands.itemReparse.mockRejectedValueOnce({ code: "NOT_FOUND", message: "not found" });
  renderPanel(<DetailPanel item={item} onClose={onClose} />);

  fireEvent.click(screen.getByRole("button", { name: "重新解析" }));
  expect(await screen.findByText("该素材已不存在")).toBeInTheDocument();
  expect(onClose).toHaveBeenCalledTimes(1);
});

test("opens the in-app overlay from the thumbnail and closes on Escape", async () => {
  renderPanel(<DetailPanel item={item} onClose={() => {}} />);
  // 等详情（含 originalUrl）到位再开覆盖层，否则标签会退成「缩略图」。
  await screen.findByText("1280 × 800");

  fireEvent.click(screen.getByRole("button", { name: "查看原图" }));

  const overlay = screen.getByRole("dialog", { name: "查看原图" });
  expect(overlay).toHaveTextContent("原图");
  // 覆盖层必须真的加载原图，而不是加载失败后回退成缩略图。
  expect(within(overlay).getByRole("img")).toHaveAttribute(
    "src",
    "http://visrecall.localhost/original/abc123",
  );
  expect(overlay).not.toHaveTextContent("原图文件缺失");

  fireEvent.keyDown(overlay, { key: "Escape" });
  await waitFor(() => {
    expect(screen.queryByRole("dialog", { name: "查看原图" })).not.toBeInTheDocument();
  });
});

test("copies the original and reports success with a toast", async () => {
  renderPanel(<DetailPanel item={item} onClose={() => {}} />);

  fireEvent.click(screen.getByRole("button", { name: "复制原图" }));
  await waitFor(() => {
    expect(commands.copyItemOriginal).toHaveBeenCalledWith("abc123");
  });
  expect(await screen.findByText("已复制原图")).toBeInTheDocument();
});

test("explains a missing original file instead of a generic error", async () => {
  commands.copyItemOriginal.mockRejectedValueOnce({
    code: "FILE_MISSING",
    message: "file missing: originals/x.png",
  });
  renderPanel(<DetailPanel item={item} onClose={() => {}} />);

  fireEvent.click(screen.getByRole("button", { name: "复制原图" }));
  expect(await screen.findByText("原图文件缺失")).toBeInTheDocument();
});

test("keeps the note text and reports failure when saving fails", async () => {
  commands.noteSet.mockRejectedValueOnce({
    code: "IO",
    message: "磁盘只读",
  });
  renderPanel(<DetailPanel item={item} onClose={() => {}} />);

  const note = screen.getByLabelText("备注");
  fireEvent.change(note, { target: { value: "写不进也要留着" } });

  expect(await screen.findByText(/保存失败/)).toBeInTheDocument();
  // 失败不回滚用户输入。
  expect(note).toHaveValue("写不进也要留着");
});

test("removes a tag through tag_detach without editing the local array", async () => {
  commands.getItemExt.mockResolvedValueOnce({
    item: {
      id: "abc123",
      kind: "image",
      status: "ready",
      capturedAt: 1_700_000_000_000,
      sourceApp: "chrome.exe",
      sourceTitle: "Test",
      sourceUrl: null,
      width: 1280,
      height: 800,
      charCount: null,
      note: "",
      thumbUrl: "visrecall://thumb/1",
    },
    extras: {
      summary: null,
      bodyText: null,
      originalUrl: "visrecall://original/abc123",
      tags: [{ id: "t1", name: "work", auto: false, hue: "jade", itemCount: 3 }],
    },
  });
  renderPanel(<DetailPanel item={item} onClose={() => {}} />);

  const chip = await screen.findByText("work");
  expect(chip.closest(".tag")).toHaveStyle({ "--hue": "var(--hue-jade)" });

  fireEvent.click(screen.getByRole("button", { name: "移除标签 work" }));
  await waitFor(() => {
    expect(commands.tagDetach).toHaveBeenCalledWith("abc123", "t1");
  });
  // 本地不删数组：等 `item_tags_updated` 事件驱动刷新。
  expect(within(chip.closest(".tag-row") as HTMLElement).getByText("work")).toBeInTheDocument();
});
