import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { TITLE_LIMIT } from "../../lib/itemText";
import { RenameItemDialog } from "./RenameItemDialog";

const renameItem = vi.hoisted(() => vi.fn(async () => ({ id: "i1" })));
vi.mock("../../lib/api", () => ({ api: { renameItem } }));
vi.mock("../../lib/toast", () => ({ pushToast: vi.fn() }));

afterEach(() => {
  vi.clearAllMocks();
});

test("prefills the current user title and saves trimmed text", async () => {
  const onClose = vi.fn();
  render(
    <RenameItemDialog
      item={{ id: "i1", title: "旧标题", sourceTitle: "来源标题" }}
      onClose={onClose}
    />,
  );

  const input = screen.getByRole("textbox");
  expect(input).toHaveValue("旧标题");

  fireEvent.change(input, { target: { value: "  新标题  " } });
  fireEvent.click(screen.getByRole("button", { name: "保存" }));

  await waitFor(() => {
    expect(renameItem).toHaveBeenCalledWith("i1", "新标题");
  });
  expect(onClose).toHaveBeenCalled();
});

test("empty text or the source title restores the fallback title", async () => {
  const first = render(
    <RenameItemDialog item={{ id: "i1", title: "旧标题", sourceTitle: "来源标题" }} onClose={() => {}} />,
  );

  const input = screen.getByRole("textbox");
  fireEvent.change(input, { target: { value: "   " } });
  expect(screen.getByText("将显示来源标题")).toBeInTheDocument();
  fireEvent.click(screen.getByRole("button", { name: "保存" }));
  await waitFor(() => expect(renameItem).toHaveBeenCalledWith("i1", null));

  // 保存成功后组件通常已卸载；这里手动卸载，避免复用「保存中」状态。
  first.unmount();
  renameItem.mockClear();

  render(
    <RenameItemDialog item={{ id: "i1", title: "旧标题", sourceTitle: "来源标题" }} onClose={() => {}} />,
  );
  fireEvent.change(screen.getByRole("textbox"), { target: { value: "来源标题" } });
  fireEvent.click(screen.getByRole("button", { name: "保存" }));
  await waitFor(() => expect(renameItem).toHaveBeenCalledWith("i1", null));
});

test("Enter saves and Escape closes without saving", async () => {
  const onClose = vi.fn();
  render(
    <RenameItemDialog
      item={{ id: "i1", title: "旧标题", sourceTitle: "来源标题" }}
      onClose={onClose}
    />,
  );

  const input = screen.getByRole("textbox");
  fireEvent.keyDown(input, { key: "Escape" });
  expect(onClose).toHaveBeenCalledTimes(1);
  expect(renameItem).not.toHaveBeenCalled();

  fireEvent.change(input, { target: { value: "新标题" } });
  fireEvent.keyDown(input, { key: "Enter" });
  await waitFor(() => expect(renameItem).toHaveBeenCalledWith("i1", "新标题"));
});

test("input is capped at the backend title limit", () => {
  render(
    <RenameItemDialog item={{ id: "i1", title: null, sourceTitle: "" }} onClose={() => {}} />,
  );

  const input = screen.getByRole("textbox") as HTMLInputElement;
  expect(input.maxLength).toBe(TITLE_LIMIT);

  fireEvent.change(input, { target: { value: "字".repeat(TITLE_LIMIT + 20) } });
  expect(input.value).toHaveLength(TITLE_LIMIT);
  expect(screen.getByText(`${TITLE_LIMIT}/${TITLE_LIMIT}`)).toBeInTheDocument();
});
