import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { Sidebar } from "./Sidebar";

const commands = vi.hoisted(() => ({
  tagsList: vi.fn(async () => [
    { id: "t1", name: "work", auto: false, hue: "jade", itemCount: 5 },
    { id: "t2", name: "参考", auto: false, hue: "ochre", itemCount: 2 },
    { id: "t3", name: "待读", auto: false, hue: "slate", itemCount: 1 },
  ]),
  storageUsageGet: vi.fn(async () => ({
    // 1.2 GiB / 50 GiB
    usedBytes: Math.round(1.2 * 1024 * 1024 * 1024),
    capacityBytes: 50 * 1024 * 1024 * 1024,
    itemCount: 20,
  })),
  tagRename: vi.fn(async () => undefined),
  tagDelete: vi.fn(async () => 3),
}));

vi.mock("../../lib/bindings", () => ({ commands }));

function renderSidebar(props: Partial<Parameters<typeof Sidebar>[0]> = {}) {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(
    <QueryClientProvider client={client}>
      <Sidebar collapsed={false} onToggle={() => {}} itemCount={7} {...props} />
    </QueryClientProvider>,
  );
}

afterEach(() => {
  vi.clearAllMocks();
});

test("shows the current view with the loaded count", () => {
  renderSidebar();

  expect(screen.getByText("全部素材")).toBeInTheDocument();
  expect(screen.getByText("7")).toBeInTheDocument();
  // 收件箱/星标/合集属 V0.5，不进 DOM。
  for (const absent of ["收件箱", "星标", "合集"]) {
    expect(screen.queryByText(absent)).not.toBeInTheDocument();
  }
});

test("renders one pill per tag with its item count", async () => {
  const { container } = renderSidebar();

  await waitFor(() => {
    expect(container.querySelectorAll(".tag-pill")).toHaveLength(3);
  });
  const pills = container.querySelectorAll(".tag-pill");
  expect(pills[0]).toHaveTextContent("work");
  expect(pills[0]).toHaveTextContent("5");
  expect(pills[1]).toHaveTextContent("2");
  expect(pills[2]).toHaveTextContent("1");
  // 阶段 03 才做筛选：pill 本身不是按钮。
  expect(pills[0].tagName).toBe("SPAN");
});

test("shows usage as used / capacity with a matching bar width", async () => {
  const { container } = renderSidebar();

  expect(await screen.findByText("已用 1.2 GB / 50.0 GB")).toBeInTheDocument();
  const bar = await screen.findByRole("progressbar", { name: "存储用量" });
  // 与组件同一口径：用真实的字节常量算比例，避免浮点误差导致断言不稳。
  const usedBytes = Math.round(1.2 * 1024 * 1024 * 1024);
  const capacityBytes = 50 * 1024 * 1024 * 1024;
  const expected = (usedBytes / capacityBytes) * 100;
  expect(Number(bar.getAttribute("aria-valuenow"))).toBe(Math.round(expected));
  expect(container.querySelector(".storage-bar i")).toHaveStyle({
    width: `${expected}%`,
  });
});

test("falls back to a dash when usage cannot be read", async () => {
  commands.storageUsageGet.mockRejectedValueOnce({ code: "IO", message: "读不到" });
  const { container } = renderSidebar();

  expect(await screen.findByText("已用 —")).toBeInTheDocument();
  expect(container.querySelector(".storage-bar i")).toHaveStyle({ width: "0%" });
});

test("renames a tag through tag_rename", async () => {
  renderSidebar();

  await screen.findByText("work");
  fireEvent.click(screen.getByRole("button", { name: "标签操作 work" }));
  fireEvent.click(screen.getByRole("menuitem", { name: "重命名" }));

  const input = screen.getByLabelText("重命名标签 work");
  fireEvent.change(input, { target: { value: "job" } });
  fireEvent.keyDown(input, { key: "Enter" });

  await waitFor(() => {
    expect(commands.tagRename).toHaveBeenCalledWith("t1", "job");
  });
});

test("deletes a tag only after confirming the affected count", async () => {
  renderSidebar();

  await screen.findByText("work");
  fireEvent.click(screen.getByRole("button", { name: "标签操作 work" }));
  fireEvent.click(screen.getByRole("menuitem", { name: "删除" }));

  const dialog = await screen.findByRole("dialog");
  expect(dialog).toHaveTextContent("将影响 5 条素材");

  fireEvent.click(screen.getByRole("button", { name: "删除标签" }));
  await waitFor(() => {
    expect(commands.tagDelete).toHaveBeenCalledWith("t1");
  });
});

test("cancelling the delete dialog keeps the tag", async () => {
  renderSidebar();

  await screen.findByText("work");
  fireEvent.click(screen.getByRole("button", { name: "标签操作 work" }));
  fireEvent.click(screen.getByRole("menuitem", { name: "删除" }));
  fireEvent.click(await screen.findByRole("button", { name: "取消" }));

  expect(commands.tagDelete).not.toHaveBeenCalled();
});

test("collapses through the toggle and reports the new state", () => {
  const onToggle = vi.fn();
  const { container } = renderSidebar({ onToggle, collapsed: true });

  expect(container.querySelector(".sidebar")).toHaveClass("collapsed");
  const toggle = screen.getByRole("button", { name: "展开侧栏" });
  expect(toggle).toHaveAttribute("aria-expanded", "false");

  fireEvent.click(toggle);
  expect(onToggle).toHaveBeenCalledTimes(1);
});

test("clicking a tag pill applies the filter and can be cancelled", async () => {
  const onTagFilter = vi.fn();
  const { container, rerender } = renderSidebar({ onTagFilter });
  await screen.findByText("work");

  const pill = container.querySelector(".tag-pill") as HTMLElement;
  expect(pill).toHaveAttribute("aria-pressed", "false");
  fireEvent.click(pill);
  expect(onTagFilter).toHaveBeenCalledWith("work");

  // 激活态由 App 回传；再点一次由 App 恢复原查询（这里只断言回调再次触发）
  const client = (container.closest("#root") ?? container) as HTMLElement;
  void client;
  rerender(
    <QueryClientProvider client={new QueryClient()}>
      <Sidebar
        collapsed={false}
        onToggle={() => {}}
        itemCount={7}
        onTagFilter={onTagFilter}
        activeTag="work"
      />
    </QueryClientProvider>,
  );
  expect(container.querySelector(".tag-pill")).toHaveClass("is-active");
  fireEvent.click(container.querySelector(".tag-pill") as HTMLElement);
  expect(onTagFilter).toHaveBeenCalledTimes(2);
});
