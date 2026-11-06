import { fireEvent, render, screen } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { Sidebar } from "./Sidebar";

function renderSidebar(props: Partial<Parameters<typeof Sidebar>[0]> = {}) {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(
    <QueryClientProvider client={client}>
      <Sidebar collapsed={false} onToggle={() => {}} itemCount={7} {...props} />
    </QueryClientProvider>,
  );
}

test("shows the current view with the loaded count", () => {
  renderSidebar();

  expect(screen.getByText("全部素材")).toBeInTheDocument();
  expect(screen.getByText("7")).toBeInTheDocument();
  // 收件箱/星标/合集属 V0.5，不进 DOM。
  for (const absent of ["收件箱", "星标", "合集"]) {
    expect(screen.queryByText(absent)).not.toBeInTheDocument();
  }
});

test("shows empty tag and storage states instead of invented numbers", () => {
  renderSidebar();

  expect(screen.getByText("暂无标签")).toBeInTheDocument();
  expect(screen.getByText("已用 —")).toBeInTheDocument();
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
