import { render, screen } from "@testing-library/react";
import { StatusBar } from "./StatusBar";

test("shows the version and only the shortcuts that actually work", () => {
  render(<StatusBar status="本地索引已就绪" version="0.1.0" />);

  expect(screen.getByText("本地索引已就绪")).toBeInTheDocument();
  expect(screen.getByText("v0.1.0")).toBeInTheDocument();
  expect(screen.getByText("T")).toBeInTheDocument();

  // T7 之前键盘绑定不存在，状态栏不能提示按了没反应的键。
  for (const hint of ["Enter", "Ctrl", "B", "↑"]) {
    expect(screen.queryByText(hint)).not.toBeInTheDocument();
  }
});
