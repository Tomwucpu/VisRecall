import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import type { ReactNode } from "react";
import { resetToasts } from "../../lib/toast";

const writeText = vi.hoisted(() => vi.fn(async () => undefined));

beforeAll(() => {
  Object.defineProperty(navigator, "clipboard", {
    value: { writeText },
    configurable: true,
  });
});
import type { ItemDetail } from "../../lib/api";
import { ToastViewport } from "../shell/ToastViewport";
import type { ItemCardData } from "../library/ItemCard";
import { DetailPanel } from "./DetailPanel";

const commands = vi.hoisted(() => {
  const getItemExt = vi.fn(async (id: string): Promise<ItemDetail> => ({
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
      thumbUrl: "visrecall://thumb/1" as string | null,
    },
    extras: {
      summary: null as string | null,
      summarySource: "none",
      ocrBoxes: [] as ItemDetail["extras"]["ocrBoxes"],
      bodyText: null as string | null,
      bodyTextTruncated: false,
      bodyTextEditable: false,
      ocrText: null as string | null,
      ocrTextTruncated: false,
      originalUrl: "visrecall://original/abc123" as string | null,
      tags: [] as { id: string; name: string; auto: boolean; hue: string | null; itemCount: number }[],
      reference: null as ItemDetail["extras"]["reference"],
    },
  }));

  return {
    getItemExt,
    noteSet: vi.fn(async () => undefined),
    itemSummarySet: vi.fn(async () => null),
    itemOcrTextSet: vi.fn(async () => null),
    itemBodyTextSet: vi.fn(async () => null),
    copyItemOriginal: vi.fn(async () => undefined),
    itemDelete: vi.fn(async () => undefined),
    itemReparse: vi.fn(async () => undefined),
    tagAdd: vi.fn(async () => ({ id: "t1", name: "work", auto: false, hue: null, itemCount: 1 })),
    tagDetach: vi.fn(async () => undefined),
    tagsList: vi.fn(async () => []),
    openReferenceFile: vi.fn(async () => null),
  };
});

test("shows reference provenance and opens the original without copying it", async () => {
  commands.getItemExt.mockResolvedValueOnce({
    item: {
      id: "abc123", kind: "image", status: "ready", capturedAt: 1_700_000_000_000,
      sourceApp: "VisRecall", sourceTitle: "引用.png", sourceUrl: null,
      width: 800, height: 600, charCount: null, note: "", thumbUrl: null,
    },
    extras: {
      summary: null, summarySource: "none", ocrBoxes: [], bodyText: null,
      bodyTextTruncated: false, bodyTextEditable: false, ocrText: null,
      ocrTextTruncated: false, originalUrl: null, tags: [],
      reference: {
        sourceId: "source-1", sourcePath: "D:\\素材", relativePath: "目录\\引用.png",
        status: "available", lastIndexedAt: 1_700_000_000_000,
      },
    },
  });
  renderPanel(<DetailPanel item={item} onClose={() => {}} />);
  expect(await screen.findByText("引用文件")).toBeInTheDocument();
  expect(screen.getByText("D:\\素材")).toBeInTheDocument();
  expect(screen.getByText("目录\\引用.png")).toBeInTheDocument();
  fireEvent.click(screen.getByRole("button", { name: "打开原文件" }));
  await waitFor(() => expect(commands.openReferenceFile).toHaveBeenCalledWith("abc123", false));
  fireEvent.click(screen.getByRole("button", { name: "打开所在目录" }));
  await waitFor(() => expect(commands.openReferenceFile).toHaveBeenCalledWith("abc123", true));
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

const baseExtras: ItemDetail["extras"] = {
  summary: null,
  summarySource: "none",
  ocrBoxes: [],
  bodyText: null,
  bodyTextTruncated: false,
  bodyTextEditable: false,
  ocrText: null,
  ocrTextTruncated: false,
  originalUrl: null,
  tags: [],
  reference: null,
};

function detailWith(
  extras: Partial<ItemDetail["extras"]> = {},
  itemOverrides: Partial<ItemDetail["item"]> = {},
): ItemDetail {
  return {
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
      thumbUrl: null,
      ...itemOverrides,
    } as ItemDetail["item"],
    extras: { ...baseExtras, ...extras },
  };
}

function renderPanel(node: ReactNode) {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(
    <QueryClientProvider client={client}>
      {node}
      <ToastViewport />
    </QueryClientProvider>,
  );
}

/** 需要 rerender 换素材时复用同一个 QueryClient（否则查询缓存被重置）。 */
function renderPanelWithClient(node: ReactNode) {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const view = render(
    <QueryClientProvider client={client}>
      {node}
      <ToastViewport />
    </QueryClientProvider>,
  );
  return {
    client,
    rerender: (next: ReactNode) =>
      view.rerender(
        <QueryClientProvider client={client}>
          {next}
          <ToastViewport />
        </QueryClientProvider>,
      ),
  };
}

afterEach(() => {
  resetToasts();
  vi.clearAllMocks();
});

test("shows the AI summary placeholder and keeps copy disabled", async () => {
  renderPanel(<DetailPanel item={item} onClose={() => {}} />);

  expect(await screen.findByText("待解析")).toBeInTheDocument();
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

test("shows the note actions only while editing or after a change", () => {
  renderPanel(<DetailPanel item={item} onClose={() => {}} />);
  const note = screen.getByLabelText("备注");

  // 没点备注框、内容也没改：按钮不占位。
  expect(screen.queryByRole("button", { name: "保存" })).not.toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "取消" })).not.toBeInTheDocument();

  fireEvent.focusIn(note);
  expect(screen.getByRole("button", { name: "保存" })).toBeInTheDocument();
  expect(screen.getByRole("button", { name: "取消" })).toBeInTheDocument();

  // 失焦且没有改动 → 收起。
  fireEvent.focusOut(note);
  expect(screen.queryByRole("button", { name: "保存" })).not.toBeInTheDocument();

  // 有未保存改动时，即使失焦也保留，否则改完就没处保存了。
  fireEvent.change(note, { target: { value: "改了" } });
  fireEvent.focusOut(note);
  expect(screen.getByRole("button", { name: "保存" })).toBeInTheDocument();
});

test("saves the note only when the save button is clicked", async () => {
  renderPanel(<DetailPanel item={item} onClose={() => {}} />);

  const note = screen.getByLabelText("备注");
  fireEvent.change(note, { target: { value: "记" } });
  fireEvent.change(note, { target: { value: "记一条备注" } });
  expect(note).toHaveValue("记一条备注");

  // 不做自动保存：改完不点「保存」就不会落盘。
  expect(commands.noteSet).not.toHaveBeenCalled();
  // 元信息行把字数与状态拼在一起（`6 / 10000 · 未保存`），因此用正则匹配。
  expect(await screen.findByText(/未保存/)).toBeInTheDocument();

  fireEvent.click(screen.getByRole("button", { name: "保存" }));

  await waitFor(() => {
    expect(commands.noteSet).toHaveBeenCalledWith("abc123", "记一条备注");
  });
  expect(await screen.findByText(/已保存/)).toBeInTheDocument();
});

test("cancelling the note restores the saved content without writing", async () => {
  renderPanel(<DetailPanel item={item} onClose={() => {}} />);

  const note = screen.getByLabelText("备注");
  fireEvent.change(note, { target: { value: "写了一半的草稿" } });
  expect(note).toHaveValue("写了一半的草稿");

  fireEvent.click(screen.getByRole("button", { name: "取消" }));

  // 默认备注是空字符串，取消即回到它。
  expect(note).toHaveValue("");
  expect(commands.noteSet).not.toHaveBeenCalled();
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
  fireEvent.click(screen.getByRole("button", { name: "保存" }));

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
      summarySource: "none",
      ocrBoxes: [],
      bodyText: null,
      bodyTextTruncated: false,
      bodyTextEditable: false,
      ocrText: null,
      ocrTextTruncated: false,
      originalUrl: "visrecall://original/abc123",
      tags: [{ id: "t1", name: "work", auto: false, hue: "jade", itemCount: 3 }],
      reference: null,
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

test("edits the AI summary only when save is clicked", async () => {
  commands.getItemExt.mockResolvedValueOnce(
    detailWith({ summary: "旧描述", summarySource: "vlm" }),
  );
  renderPanel(<DetailPanel item={item} onClose={() => {}} />);
  await waitFor(() =>
    expect(screen.getByLabelText("编辑 AI 描述")).toHaveValue("旧描述"),
  );

  // 与备注同构：textarea 常驻，直接可写，不再有「编辑」按钮。
  const area = screen.getByLabelText("编辑 AI 描述");
  fireEvent.change(area, { target: { value: "新描述" } });
  // 显式保存：只改草稿不发请求。
  expect(commands.itemSummarySet).not.toHaveBeenCalled();

  fireEvent.click(screen.getByRole("button", { name: "保存 AI 描述" }));
  await waitFor(() =>
    expect(commands.itemSummarySet).toHaveBeenCalledWith("abc123", "新描述"),
  );
});

test("cancelling the summary edit restores the saved text without writing", async () => {
  commands.getItemExt.mockResolvedValueOnce(
    detailWith({ summary: "旧描述", summarySource: "vlm" }),
  );
  renderPanel(<DetailPanel item={item} onClose={() => {}} />);
  await waitFor(() =>
    expect(screen.getByLabelText("编辑 AI 描述")).toHaveValue("旧描述"),
  );

  const area = screen.getByLabelText("编辑 AI 描述");
  fireEvent.change(area, { target: { value: "不要保存的描述" } });
  fireEvent.click(screen.getByRole("button", { name: "取消编辑 AI 描述" }));

  // 取消恢复已保存内容，textarea 常驻。
  expect(area).toHaveValue("旧描述");
  expect(commands.itemSummarySet).not.toHaveBeenCalled();
});

test("edits the body text of a text item", async () => {
  commands.getItemExt.mockResolvedValueOnce(
    detailWith({ bodyText: "旧正文", bodyTextEditable: true }, { kind: "text" }),
  );
  renderPanel(<DetailPanel item={{ ...item, kind: "text" }} onClose={() => {}} />);
  await waitFor(() =>
    expect(screen.getByLabelText("编辑原文")).toHaveValue("旧正文"),
  );

  fireEvent.change(screen.getByLabelText("编辑原文"), {
    target: { value: "新正文" },
  });
  fireEvent.click(screen.getByRole("button", { name: "保存原文" }));

  await waitFor(() =>
    expect(commands.itemBodyTextSet).toHaveBeenCalledWith("abc123", "新正文"),
  );
});

test("keeps an external reference body read-only", async () => {
  commands.getItemExt.mockResolvedValueOnce(
    detailWith({ bodyText: "外部正文", bodyTextEditable: false }, { kind: "text" }),
  );
  renderPanel(<DetailPanel item={{ ...item, kind: "text" }} onClose={() => {}} />);

  expect(await screen.findByText("外部正文")).toBeInTheDocument();
  // 外部引用没有编辑入口，也不渲染 textarea。
  expect(screen.queryByLabelText("编辑原文")).not.toBeInTheDocument();
  expect(screen.getByText("该素材引用外部文件，原文只读。")).toBeInTheDocument();
});

test("hides the body editor when the original was truncated", async () => {
  commands.getItemExt.mockResolvedValueOnce(
    detailWith(
      { bodyText: "很长很长的原文…（已截断）", bodyTextEditable: true, bodyTextTruncated: true },
      { kind: "text" },
    ),
  );
  renderPanel(<DetailPanel item={{ ...item, kind: "text" }} onClose={() => {}} />);

  expect(await screen.findByText(/已截断/)).toBeInTheDocument();
  expect(screen.queryByLabelText("编辑原文")).not.toBeInTheDocument();
  expect(screen.getByText(/暂不支持在应用内编辑/)).toBeInTheDocument();
});

test("edits the ocr text of an image item", async () => {
  commands.getItemExt.mockResolvedValueOnce(
    detailWith({ ocrText: "模型识别的文字", ocrTextTruncated: false }),
  );
  renderPanel(<DetailPanel item={item} onClose={() => {}} />);
  await waitFor(() =>
    expect(screen.getByLabelText("编辑识别文字")).toHaveValue("模型识别的文字"),
  );

  fireEvent.change(screen.getByLabelText("编辑识别文字"), {
    target: { value: "改后的文字" },
  });
  fireEvent.click(screen.getByRole("button", { name: "保存识别文字" }));

  await waitFor(() =>
    expect(commands.itemOcrTextSet).toHaveBeenCalledWith("abc123", "改后的文字"),
  );
});

test("typing after a successful save returns to the unsaved state", async () => {
  commands.getItemExt.mockResolvedValueOnce(
    detailWith({ summary: "旧描述", summarySource: "vlm" }),
  );
  renderPanel(<DetailPanel item={item} onClose={() => {}} />);
  await waitFor(() =>
    expect(screen.getByLabelText("编辑 AI 描述")).toHaveValue("旧描述"),
  );

  fireEvent.change(screen.getByLabelText("编辑 AI 描述"), {
    target: { value: "第一次改" },
  });
  fireEvent.click(screen.getByRole("button", { name: "保存 AI 描述" }));
  await waitFor(() =>
    expect(commands.itemSummarySet).toHaveBeenCalledWith("abc123", "第一次改"),
  );
  expect(await screen.findByText(/已保存/)).toBeInTheDocument();

  // 保存后再改一个字：必须回到「未保存」，否则会误导用户带着改动离开。
  fireEvent.change(screen.getByLabelText("编辑 AI 描述"), {
    target: { value: "第二次改" },
  });
  expect(screen.queryByText(/已保存/)).not.toBeInTheDocument();
  expect(screen.getByText(/未保存/)).toBeInTheDocument();
});

test("keeps the summary text and reports failure when saving fails", async () => {
  commands.itemSummarySet.mockRejectedValueOnce({
    code: "IO",
    message: "磁盘只读",
  });
  commands.getItemExt.mockResolvedValueOnce(
    detailWith({ summary: "旧描述", summarySource: "vlm" }),
  );
  renderPanel(<DetailPanel item={item} onClose={() => {}} />);
  await waitFor(() =>
    expect(screen.getByLabelText("编辑 AI 描述")).toHaveValue("旧描述"),
  );

  const area = screen.getByLabelText("编辑 AI 描述");
  fireEvent.change(area, { target: { value: "写不进也要留着" } });
  fireEvent.click(screen.getByRole("button", { name: "保存 AI 描述" }));

  expect(await screen.findByText(/保存失败/)).toBeInTheDocument();
  expect(area).toHaveValue("写不进也要留着");
});

test("counts the summary limit by code point and blocks overflow", async () => {
  commands.getItemExt.mockResolvedValueOnce(
    detailWith({ summary: "旧描述", summarySource: "vlm" }),
  );
  renderPanel(<DetailPanel item={item} onClose={() => {}} />);
  await waitFor(() =>
    expect(screen.getByLabelText("编辑 AI 描述")).toHaveValue("旧描述"),
  );

  const area = screen.getByLabelText("编辑 AI 描述") as HTMLTextAreaElement;
  fireEvent.change(area, { target: { value: "😀".repeat(2_001) } });

  // emoji 在 UTF-16 里占 2 个单元，必须按码点计数（与后端 chars().count 一致）。
  expect(Array.from(area.value)).toHaveLength(2_000);
});

test("resets the content draft when switching to another item", async () => {
  const other: ItemCardData = { ...item, id: "def456", sourceTitle: "另一条" };
  commands.getItemExt.mockResolvedValueOnce(
    detailWith({ summary: "第一条描述", summarySource: "vlm" }),
  );
  const view = renderPanelWithClient(<DetailPanel item={item} onClose={() => {}} />);
  await waitFor(() =>
    expect(screen.getByLabelText("编辑 AI 描述")).toHaveValue("第一条描述"),
  );

  fireEvent.change(screen.getByLabelText("编辑 AI 描述"), {
    target: { value: "未保存的草稿" },
  });

  commands.getItemExt.mockResolvedValueOnce(
    detailWith({ summary: "第二条描述", summarySource: "vlm" }, { id: "def456" }),
  );
  view.rerender(<DetailPanel item={other} onClose={() => {}} />);

  // 草稿被丢弃，回填新素材的摘要。
  await waitFor(() =>
    expect(screen.getByLabelText("编辑 AI 描述")).toHaveValue("第二条描述"),
  );
});

test("hides the ocr editor when the text was truncated", async () => {
  commands.getItemExt.mockResolvedValueOnce(
    detailWith({ ocrText: "很长很长的识别文字…", ocrTextTruncated: true }),
  );
  renderPanel(<DetailPanel item={item} onClose={() => {}} />);

  expect(await screen.findByText("很长很长的识别文字…")).toBeInTheDocument();
  expect(screen.queryByLabelText("编辑识别文字")).not.toBeInTheDocument();
  expect(screen.getByText(/识别文字超过/)).toBeInTheDocument();
});

test("keeps an empty body editable", async () => {
  commands.getItemExt.mockResolvedValueOnce(
    detailWith({ bodyText: "", bodyTextEditable: true }, { kind: "text" }),
  );
  renderPanel(<DetailPanel item={{ ...item, kind: "text" }} onClose={() => {}} />);

  await waitFor(() => expect(screen.getByLabelText("编辑原文")).toHaveValue(""));
});

test("groups the summary copy action on the right and hides editing while unparsed", async () => {
  renderPanel(<DetailPanel item={item} onClose={() => {}} />);
  await screen.findByText("待解析");

  const copy = screen.getByRole("button", { name: "复制" });
  const group = copy.closest(".d-sec-actions") as HTMLElement | null;

  // 复制仍在右侧按钮组里，不会因为缺少徽标被 space-between 拆到中间。
  expect(group).not.toBeNull();
  expect(group?.parentElement).toHaveClass("d-sec-head");
  // 待解析（识别状态）不提供编辑入口。
  expect(screen.queryByLabelText("编辑 AI 描述")).not.toBeInTheDocument();
});

test("hides the summary actions after blur when nothing changed", async () => {
  commands.getItemExt.mockResolvedValueOnce(
    detailWith({ summary: "旧描述", summarySource: "vlm" }),
  );
  renderPanel(<DetailPanel item={item} onClose={() => {}} />);
  await waitFor(() =>
    expect(screen.getByLabelText("编辑 AI 描述")).toHaveValue("旧描述"),
  );

  const area = screen.getByLabelText("编辑 AI 描述");
  fireEvent.focus(area);
  expect(screen.getByRole("button", { name: "保存 AI 描述" })).toBeInTheDocument();

  fireEvent.blur(area);
  await waitFor(() =>
    expect(screen.queryByRole("button", { name: "保存 AI 描述" })).not.toBeInTheDocument(),
  );
  expect(commands.itemSummarySet).not.toHaveBeenCalled();
});

test("keeps the summary actions after blur when there are changes", async () => {
  commands.getItemExt.mockResolvedValueOnce(
    detailWith({ summary: "旧描述", summarySource: "vlm" }),
  );
  renderPanel(<DetailPanel item={item} onClose={() => {}} />);
  await waitFor(() =>
    expect(screen.getByLabelText("编辑 AI 描述")).toHaveValue("旧描述"),
  );

  const area = screen.getByLabelText("编辑 AI 描述");
  fireEvent.change(area, { target: { value: "改了一点" } });
  fireEvent.blur(area);

  // 有未保存改动时按钮保留，避免草稿被静默丢弃（与备注一致）。
  expect(screen.getByRole("button", { name: "保存 AI 描述" })).toBeInTheDocument();
  expect(area).toHaveValue("改了一点");
});

test("hides the ocr actions after blur when nothing changed", async () => {
  commands.getItemExt.mockResolvedValueOnce(
    detailWith({ ocrText: "模型识别的文字", ocrTextTruncated: false }),
  );
  renderPanel(<DetailPanel item={item} onClose={() => {}} />);
  await waitFor(() =>
    expect(screen.getByLabelText("编辑识别文字")).toHaveValue("模型识别的文字"),
  );

  const area = screen.getByLabelText("编辑识别文字");
  fireEvent.focus(area);
  expect(screen.getByRole("button", { name: "保存识别文字" })).toBeInTheDocument();

  fireEvent.blur(area);
  await waitFor(() =>
    expect(screen.queryByRole("button", { name: "保存识别文字" })).not.toBeInTheDocument(),
  );
  expect(commands.itemOcrTextSet).not.toHaveBeenCalled();
});

test("copies the body text", async () => {
  commands.getItemExt.mockResolvedValueOnce(
    detailWith({ bodyText: "正文内容", bodyTextEditable: true }, { kind: "text" }),
  );
  renderPanel(<DetailPanel item={{ ...item, kind: "text" }} onClose={() => {}} />);
  await waitFor(() =>
    expect(screen.getByLabelText("编辑原文")).toHaveValue("正文内容"),
  );

  fireEvent.click(screen.getByRole("button", { name: "复制正文" }));

  await waitFor(() => expect(writeText).toHaveBeenCalledWith("正文内容"));
  expect(await screen.findByText("已复制原文")).toBeInTheDocument();
});

test("copies the ocr text", async () => {
  commands.getItemExt.mockResolvedValueOnce(detailWith({ ocrText: "识别内容" }));
  renderPanel(<DetailPanel item={item} onClose={() => {}} />);
  await waitFor(() =>
    expect(screen.getByLabelText("编辑识别文字")).toHaveValue("识别内容"),
  );

  fireEvent.click(screen.getByRole("button", { name: "复制识别文字" }));

  await waitFor(() => expect(writeText).toHaveBeenCalledWith("识别内容"));
  expect(await screen.findByText("已复制识别文字")).toBeInTheDocument();
});

test("keeps a missing body editable and disables copy", async () => {
  commands.getItemExt.mockResolvedValueOnce(
    detailWith({ bodyText: null, bodyTextEditable: true }, { kind: "text" }),
  );
  renderPanel(<DetailPanel item={{ ...item, kind: "text" }} onClose={() => {}} />);

  await waitFor(() => expect(screen.getByLabelText("编辑原文")).toHaveValue(""));
  expect(screen.getByText(/可直接在上面补录/)).toBeInTheDocument();
  expect(screen.getByRole("button", { name: "复制正文" })).toBeDisabled();
});

test("keeps the summary editor always mounted like the note", async () => {
  commands.getItemExt.mockResolvedValueOnce(
    detailWith({ summary: "旧描述", summarySource: "vlm" }),
  );
  renderPanel(<DetailPanel item={item} onClose={() => {}} />);
  await waitFor(() =>
    expect(screen.getByLabelText("编辑 AI 描述")).toHaveValue("旧描述"),
  );

  const area = screen.getByLabelText("编辑 AI 描述");
  expect(area).toHaveValue("旧描述");
  // 没有动作时不预留按钮行高度（与备注的 .has-actions 一致）。
  expect(area.closest(".d-note-wrap")).not.toHaveClass("has-actions");
  expect(screen.queryByTitle("修改这段 AI 描述")).not.toBeInTheDocument();
});
