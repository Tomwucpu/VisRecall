import { render, screen } from "@testing-library/react";

import { ExplainBar } from "./ExplainBar";
import { hitSource, hitSourceLabel, hitSourceSentence } from "../../lib/searchTypes";
import { QueryChips } from "./QueryChips";
import { ResultGrid } from "./ResultGrid";
import { stripChip, type SearchHit } from "../../lib/searchTypes";

const baseExplanation = {
  filters: [],
  terms: [],
  semantic: false,
  highlightBoxes: [],
};

test("explains the three hit sources", () => {
  const { rerender } = render(<ExplainBar explanation={baseExplanation} />);
  expect(screen.getByText("仅满足筛选条件")).toBeInTheDocument();

  rerender(
    <ExplainBar
      explanation={{ ...baseExplanation, terms: ["发票"], highlightBoxes: [
        { text: "发票号码", x: 0.1, y: 0.2, w: 0.3, h: 0.05 },
      ] }}
    />,
  );
  expect(screen.getByText("命中文字：发票")).toBeInTheDocument();
  expect(screen.getByText("1 处文字高亮")).toBeInTheDocument();

  rerender(
    <ExplainBar
      explanation={{ ...baseExplanation, semantic: true, filters: ["橙色"] }}
    />,
  );
  expect(screen.getByText("按语义相关性匹配")).toBeInTheDocument();
  expect(screen.getByText("橙色")).toBeInTheDocument();
});

test("source helpers stay consistent", () => {
  expect(hitSource(baseExplanation)).toBe("filter");
  expect(hitSource({ ...baseExplanation, terms: ["a"] })).toBe("fts");
  expect(hitSource({ ...baseExplanation, semantic: true, terms: ["a"] })).toBe("semantic");
  expect(hitSourceLabel("semantic")).toBe("语义命中");
  expect(hitSourceSentence("fts", ["发票", "金额"])).toContain("发票、金额");
});

test("renders chips and removes the chosen condition from the raw query", () => {
  const removed: string[] = [];
  render(
    <QueryChips
      parsed={{
        time: null,
        app: "chrome",
        color: ["orange"],
        chart: null,
        contentKind: null,
        hasFormula: null,
        tags: ["工作"],
        semanticText: "发票",
        chips: ["橙色", "来自 chrome", "#工作"],
      }}
      onRemove={(chip) => removed.push(chip)}
    />,
  );
  expect(screen.getByText("橙色")).toBeInTheDocument();
  fireEventClick(screen.getByLabelText("移除条件 橙色"));
  expect(removed).toEqual(["橙色"]);
});

function fireEventClick(element: HTMLElement) {
  element.dispatchEvent(new MouseEvent("click", { bubbles: true }));
}

test("stripChip removes the matching fragment and keeps the rest", () => {
  expect(stripChip("橙色 发票", "橙色")).toBe("发票");
  expect(stripChip("来自 chrome 报错", "来自 chrome")).toBe("报错");
  expect(stripChip("发票 #工作", "#工作")).toBe("发票");
  // 删不掉时原样返回（避免把查询误清空）
  expect(stripChip("发票", "时间")).toBe("发票");
});

const hit = (overrides: Partial<SearchHit> = {}): SearchHit => ({
  item: {
    id: "1",
    kind: "image",
    status: "ready",
    capturedAt: 1_700_000_000_000,
    sourceApp: "chrome",
    sourceTitle: "发票页面",
    sourceUrl: null,
    width: 1920,
    height: 1080,
    charCount: null,
    note: "",
    thumbUrl: null,
    primaryColor: "orange",
  },
  score: 0.5,
  explanation: { ...baseExplanation, terms: ["发票"] },
  ...overrides,
});

test("result grid renders hits, badge, explanation and the not-this action", () => {
  const notThis: SearchHit[] = [];
  render(
    <ResultGrid
      hits={[hit()]}
      status="ready"
      errorMessage={null}
      semanticReady
      total={1}
      onOpen={() => {}}
      onNotThis={(candidate) => notThis.push(candidate)}
      selectedId={null}
    />,
  );
  expect(screen.getByText("发票页面")).toBeInTheDocument();
  expect(screen.getByText("字面命中")).toBeInTheDocument();
  expect(screen.getByText("命中 1 项")).toBeInTheDocument();

  fireEventClick(screen.getByText("不是这个"));
  expect(notThis).toHaveLength(1);
});

test("result grid explains empty and degraded states", () => {
  const { rerender } = render(
    <ResultGrid
      hits={[]}
      status="ready"
      errorMessage={null}
      semanticReady={false}
      total={0}
      onOpen={() => {}}
      onNotThis={() => {}}
      selectedId={null}
    />,
  );
  expect(screen.getByText("没有符合条件的素材。")).toBeInTheDocument();
  expect(screen.getByText(/语义检索当前不可用/)).toBeInTheDocument();

  rerender(
    <ResultGrid
      hits={[]}
      status="error"
      errorMessage="数据库繁忙"
      semanticReady
      total={0}
      onOpen={() => {}}
      onNotThis={() => {}}
      selectedId={null}
    />,
  );
  expect(screen.getByRole("alert")).toHaveTextContent("检索失败：数据库繁忙");
});
