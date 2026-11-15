import { render, screen } from "@testing-library/react";

import { ExplainBar } from "./ExplainBar";
import {
  hitSource,
  hitSourceLabel,
  hitSourceSentence,
  literalizeChip,
  isSoftChip,
  softWordOf,
  stripChip,
  type SearchHit,
} from "../../lib/searchTypes";
import { QueryChips } from "./QueryChips";
import { ResultGrid } from "./ResultGrid";

const baseExplanation = {
  filters: [],
  facets: [],
  terms: [],
  semantic: false,
  highlightBoxes: [],
};

test("explains hard filters, soft facets and literal hits separately", () => {
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

  // 软命中单独一行，且不混进 filters
  rerender(
    <ExplainBar explanation={{ ...baseExplanation, facets: ["主色 红"] }} />,
  );
  expect(screen.getByText("参考条件命中：主色 红")).toBeInTheDocument();
  expect(screen.getByText("主色 红")).toBeInTheDocument();
  expect(screen.getByRole("note")).toHaveClass("is-facet");

  rerender(
    <ExplainBar
      explanation={{ ...baseExplanation, semantic: true, filters: ["#工作"] }}
    />,
  );
  expect(screen.getByText("按语义相关性匹配")).toBeInTheDocument();
  expect(screen.getByText("#工作")).toBeInTheDocument();
});

test("source helpers stay consistent", () => {
  expect(hitSource(baseExplanation)).toBe("filter");
  expect(hitSource({ ...baseExplanation, terms: ["a"] })).toBe("fts");
  expect(hitSource({ ...baseExplanation, facets: ["主色 红"] })).toBe("facet");
  expect(hitSource({ ...baseExplanation, semantic: true, terms: ["a"] })).toBe("semantic");
  expect(hitSourceLabel("semantic")).toBe("语义命中");
  expect(hitSourceLabel("facet")).toBe("软命中");
  expect(hitSourceSentence("fts", ["发票", "金额"])).toContain("发票、金额");
  expect(hitSourceSentence("facet", [], ["类型 图片"])).toContain("类型 图片");
});

test("renders only removable chips and removes the chosen condition", () => {
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
        semanticText: "橙色 发票",
        chips: ["来自 chrome", "#工作"],
        softChips: ["橙色(参考)"],
        facetWords: ["橙色"],
      }}
      onRemove={(chip) => removed.push(chip)}
    />,
  );
  expect(screen.getByText("#工作")).toBeInTheDocument();
  // 软命中不做成标签：只加分不排除，没有可「取消」的东西
  expect(screen.queryByText("橙色(参考)")).not.toBeInTheDocument();
  expect(screen.queryByText("仅文字")).not.toBeInTheDocument();
  fireEventClick(screen.getByLabelText("移除条件 #工作"));
  expect(removed).toEqual(["#工作"]);
});

test("time condition renders as a fixed hint without a remove button", () => {
  render(
    <QueryChips
      parsed={{
        time: { from: Date.UTC(2026, 10, 15, 4), to: Date.UTC(2026, 10, 16, 4) },
        app: null,
        color: [],
        chart: null,
        contentKind: null,
        hasFormula: null,
        tags: [],
        semanticText: "",
        chips: [],
        softChips: [],
        facetWords: [],
      }}
      onRemove={() => {
        throw new Error("时间条件不该有移除入口");
      }}
    />,
  );
  const chip = screen.getByText(/^时间：/).closest("li");
  expect(chip).toHaveClass("is-fixed");
  // 删不掉就不给 ×：避免「点了没反应」的空操作
  expect(screen.queryByLabelText(/移除条件 时间/)).not.toBeInTheDocument();
});

function fireEventClick(element: HTMLElement) {
  element.dispatchEvent(new MouseEvent("click", { bubbles: true }));
}

test("stripChip removes the matching fragment and keeps the rest", () => {
  expect(stripChip("橙色 发票", "橙色")).toBe("发票");
  expect(stripChip("来自 chrome 报错", "来自 chrome")).toBe("报错");
  expect(stripChip("发票 #工作", "#工作")).toBe("发票");
  expect(stripChip("发票 tag:工作", "#工作")).toBe("发票");
  // 删不掉时原样返回（避免把查询误清空）
  expect(stripChip("发票", "时间")).toBe("发票");
});

test("literalizeChip turns a soft word into a quoted phrase", () => {
  expect(isSoftChip("红色(参考)")).toBe(true);
  expect(isSoftChip("#工作")).toBe(false);
  expect(softWordOf("红色(参考)")).toBe("红色");
  expect(literalizeChip("红色预警", "红色(参考)")).toBe('"红色"预警');
  // 已经在引号里就不重复包
  expect(literalizeChip('"红色"预警', "红色(参考)")).toBe('"红色"预警');
  // 词不在原文里（旧记录/兜底 chip）：原样返回
  expect(literalizeChip("发票", "橙色(参考)")).toBe("发票");
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

test("result grid renders hits, badge and explanation", () => {
  render(
    <ResultGrid
      hits={[hit()]}
      status="ready"
      errorMessage={null}
      semanticReady
      total={1}
      onOpen={() => {}}
      selectedId={null}
    />,
  );
  expect(screen.getByText("发票页面")).toBeInTheDocument();
  expect(screen.getByText("字面命中")).toBeInTheDocument();
  expect(screen.getByText("命中 1 项")).toBeInTheDocument();
  expect(screen.queryByText("不是这个")).not.toBeInTheDocument();
});

test("result grid gives the card a loadable thumbnail url", () => {
  const candidate = hit();
  candidate.item.thumbUrl = "visrecall://thumb/1";

  render(
    <ResultGrid
      hits={[candidate]}
      status="ready"
      errorMessage={null}
      semanticReady
      total={1}
      onOpen={() => {}}
      selectedId={null}
    />,
  );

  // 后端给的是逻辑协议地址，WebView 只能加载 `http://visrecall.localhost/...`。
  expect(screen.getByRole("img", { name: "图片素材缩略图" })).toHaveAttribute(
    "src",
    "http://visrecall.localhost/thumb/1",
  );
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
      selectedId={null}
    />,
  );
  expect(screen.getByRole("alert")).toHaveTextContent("检索失败：数据库繁忙");
});
