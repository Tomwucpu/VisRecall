import { fireEvent, render, screen } from "@testing-library/react";
import { useState } from "react";

import { SearchBar } from "./SearchBar";

/** 受控输入需要在测试里给一个会写回 value 的宿主。 */
function Harness({ onValue }: { onValue?: (value: string) => void }) {
  const [value, setValue] = useState("");
  return (
    <SearchBar
      value={value}
      onChange={(next) => {
        setValue(next);
        onValue?.(next);
      }}
      busy={false}
      semanticReady={false}
    />
  );
}

test("types into the search box and reports each value", () => {
  const seen: string[] = [];
  render(<Harness onValue={(value) => seen.push(value)} />);
  const input = screen.getByLabelText("搜索素材");
  fireEvent.change(input, { target: { value: "发票" } });
  expect(input).toHaveValue("发票");
  expect(seen).toEqual(["发票"]);
});

test("clears with the clear button and with Escape", () => {
  render(<Harness />);
  const input = screen.getByLabelText("搜索素材");
  fireEvent.change(input, { target: { value: "橙色 漏斗图" } });
  fireEvent.click(screen.getByLabelText("清空搜索"));
  expect(input).toHaveValue("");

  fireEvent.change(input, { target: { value: "发票" } });
  fireEvent.keyDown(input, { key: "Escape" });
  expect(input).toHaveValue("");
});

test("focuses on Ctrl+K and shows the degraded hint when semantics is off", () => {
  render(<Harness />);
  const input = screen.getByLabelText("搜索素材");
  expect(input).not.toHaveFocus();
  fireEvent.keyDown(window, { key: "k", ctrlKey: true });
  expect(input).toHaveFocus();
  expect(screen.getByText("语义未就绪")).toBeInTheDocument();
});
