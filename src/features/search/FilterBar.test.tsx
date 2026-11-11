import { fireEvent, render, screen } from "@testing-library/react";
import { FilterBar } from "./FilterBar";
import { useItemFilters } from "./useItemFilters";

/** 用一个真实 controller 包住筛选条，断言的是「用户操作 → 条件」而非内部实现。 */
function Harness({ tagOptions = ["工作", "财务"] }: { tagOptions?: string[] }) {
  const controller = useItemFilters();
  return (
    <>
      <FilterBar controller={controller} tagOptions={tagOptions} />
      <output data-testid="chips">
        {controller.chips.map((chip) => chip.label).join("|")}
      </output>
    </>
  );
}

test("kind pill toggles a value and shows a count badge", () => {
  render(<Harness />);

  fireEvent.click(screen.getByRole("button", { name: /类型/ }));
  const panel = screen.getByRole("dialog", { name: "类型筛选" });
  fireEvent.click(screen.getByLabelText("图片", { exact: false }));

  expect(screen.getByTestId("chips")).toHaveTextContent("图片");
  expect(screen.getByRole("button", { name: /类型/ })).toHaveTextContent("1");

  // 面板保持打开，再点一次取消。
  expect(panel).toBeInTheDocument();
  fireEvent.click(screen.getByLabelText("图片", { exact: false }));
  expect(screen.getByTestId("chips")).toHaveTextContent("");
});

test("time preset applies a range and closes the panel", () => {
  render(<Harness />);

  fireEvent.click(screen.getByRole("button", { name: /时间/ }));
  fireEvent.click(screen.getByRole("button", { name: "近 7 天" }));

  expect(screen.getByTestId("chips")).toHaveTextContent("近 7 天");
  expect(screen.queryByRole("dialog", { name: "时间筛选" })).not.toBeInTheDocument();
});

test("custom dates keep the panel open and produce a range chip", () => {
  render(<Harness />);

  fireEvent.click(screen.getByRole("button", { name: /时间/ }));
  fireEvent.change(screen.getByLabelText("开始日期"), { target: { value: "2026-01-01" } });
  fireEvent.change(screen.getByLabelText("结束日期"), { target: { value: "2026-01-31" } });

  expect(screen.getByTestId("chips")).toHaveTextContent("2026-01-01 至 2026-01-31");
});

test("tags are multi-select and can be cleared from the panel", () => {
  render(<Harness />);

  fireEvent.click(screen.getByRole("button", { name: /标签/ }));
  fireEvent.click(screen.getByLabelText("#工作"));
  fireEvent.click(screen.getByLabelText("#财务"));
  expect(screen.getByTestId("chips")).toHaveTextContent("#工作|#财务");

  fireEvent.click(screen.getByRole("button", { name: "清空" }));
  expect(screen.getByTestId("chips")).toHaveTextContent("");
});

test("Escape closes the panel and returns focus to the pill", () => {
  render(<Harness />);

  const pill = screen.getByRole("button", { name: /状态/ });
  fireEvent.click(pill);
  expect(screen.getByRole("dialog", { name: "状态筛选" })).toBeInTheDocument();

  fireEvent.keyDown(screen.getByRole("dialog", { name: "状态筛选" }), { key: "Escape" });
  expect(screen.queryByRole("dialog", { name: "状态筛选" })).not.toBeInTheDocument();
  expect(pill).toHaveFocus();
});

test("empty tag list shows a placeholder instead of an empty panel", () => {
  render(<Harness tagOptions={[]} />);

  fireEvent.click(screen.getByRole("button", { name: /标签/ }));
  expect(screen.getByText("还没有标签")).toBeInTheDocument();
});
