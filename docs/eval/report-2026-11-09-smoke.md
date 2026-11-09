# 首轮冒烟（空库，不含评测集结论）

## 环境

- 评测库：`C:\Users\Tomwu\AppData\Local\Temp\visrecall-eval-empty`（素材 0 条）
- 嵌入模型：`bge-small-zh-v1.5-int8`（512 维）
- RRF：k=60 weights=[1.0, 1.0]
- 评测集：`docs/eval/queries.template.jsonl`
- 平台：windows x86_64

## 总览

| 指标 | 值 | 门禁 |
|---|---|---|
| 查询数 | 3 | — |
| Top-3 命中率 | 100.0% | ≥80% 通过 |
| Recall@10 | 1.000 | — |
| mAP | 1.000 | — |
| 延迟 P95 | 331 ms | 目标 ≤200ms |
| 延迟均值 | 112.3 ms | — |
| 语义通道启用 | 3/3 条查询 | — |

## 分桶

| 桶 | 查询数 | Top-3 | Recall@10 | mAP | P95(ms) |
|---|---|---|---|---|---|
| literal | 1 | 100.0% | 1.000 | 1.000 | 3 |
| scene | 1 | 100.0% | 1.000 | 1.000 | 3 |
| screenshot | 1 | 100.0% | 1.000 | 1.000 | 331 |

## 通道分布（按命中条数）

| 通道 | 命中条数 |
|---|---|

## Top-3 未命中的查询

无。

## 调参回路

首跑作为基线。若 Top-3 <80%，按以下顺序尝试并各出一份对比报告（含 diff 摘要）：
1. jieba 词典（补领域词，见 `crates/search/src/tokenizer.rs` 的 `DOMAIN_WORDS`）；
2. RRF 权重（`settings['search.rrf']`）与 `execute::SEMANTIC_RELATIVE_FLOOR`；
3. 记录「是否需要升级 bge-base-zh」的评审结论（不擅自升级）。
