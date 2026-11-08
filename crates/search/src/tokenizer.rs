//! 分词：**本 crate 是 jieba 的唯一入口**（design §4.4）。
//!
//! 写入侧（`pipeline` 构造 `items_fts` 内容）与查询侧（`parser` / `fts_query`）必须
//! 用同一个分词器与同一个版本，否则「写进去的词查不回来」。`TOKENIZER_VERSION`
//! 因此放在 `domain`（`library` 启动自检也要读），这里只做 re-export。

use std::sync::OnceLock;

use jieba_rs::Jieba;

pub use visrecall_domain::search::TOKENIZER_VERSION;

/// 全局词典实例：jieba 初始化（加载词典）约 100–300ms，必须只做一次。
static JIEBA: OnceLock<Jieba> = OnceLock::new();

fn jieba() -> &'static Jieba {
    JIEBA.get_or_init(|| {
        let mut instance = Jieba::new();
        // 内置领域词：jieba 默认词典把它们切成单字（如「营收」→「营」「收」），
        // 单字 token 噪声极大（几乎命中所有含该字的素材）。P3-R14 的调参回路第一步
        // 就是调词典，这里先固化一批截图场景高频词；只增词长 ≥2 且不是更长词前缀的
        // 词（例如不加「发票号」，否则查「发票」会漏掉它）。
        for word in DOMAIN_WORDS {
            instance.add_word(word, None, None);
        }
        instance
    })
}

/// 领域词表（截图 UI 与查询里的高频复合词）。
const DOMAIN_WORDS: &[&str] = &[
    "营收",
    "高亮",
    "看板",
    "漏斗图",
    "饼图",
    "大盘",
    "置信度",
    "弹窗",
    "埋点",
    "灰度",
    "工单",
    "报表",
    "账单",
    "退款",
    "结算",
    "库存",
    "日志",
    "堆栈",
    "断点",
    "分支",
    "仓库",
    "提交",
    "筛选",
    "排序",
    "分页",
    "侧栏",
    "顶栏",
    "弹层",
    "浮层",
    "水印",
    "缩略图",
    "预览",
];

/// 中文停用词（虚词/代词/量词等）：查询词里出现它们只会稀释 BM25。
const STOPWORDS: &[&str] = &[
    "的",
    "了",
    "和",
    "与",
    "及",
    "或",
    "是",
    "在",
    "有",
    "我",
    "你",
    "他",
    "她",
    "它",
    "我们",
    "你们",
    "他们",
    "这",
    "那",
    "这个",
    "那个",
    "这些",
    "那些",
    "什么",
    "怎么",
    "怎样",
    "如何",
    "为什么",
    "哪",
    "哪个",
    "哪里",
    "一个",
    "一些",
    "一下",
    "一点",
    "很",
    "太",
    "更",
    "最",
    "都",
    "也",
    "还",
    "就",
    "又",
    "再",
    "才",
    "只",
    "被",
    "把",
    "从",
    "到",
    "对",
    "给",
    "让",
    "使",
    "为",
    "以",
    "于",
    "关于",
    "根据",
    "按照",
    "通过",
    "以及",
    "并且",
    "但是",
    "不过",
    "然后",
    "如果",
    "因为",
    "所以",
    "可以",
    "能",
    "会",
    "要",
    "想",
    "需要",
    "应该",
    "可能",
    "已经",
    "正在",
    "将",
    "着",
    "过",
    "吧",
    "吗",
    "呢",
    "啊",
    "呀",
    "哦",
    "嗯",
    "上",
    "下",
    "里",
    "中",
    "内",
    "外",
    "前",
    "后",
    "left",
    "right",
    "the",
    "a",
    "an",
    "of",
    "to",
    "in",
    "on",
    "and",
    "or",
    "is",
    "are",
    "was",
    "were",
    "be",
    "for",
    "with",
    "at",
    "by",
    "from",
    "as",
    "it",
    "this",
    "that",
    "these",
    "those",
];

/// 判断 token 是否值得进索引/查询（含字母数字，非纯标点/空白）。
fn is_word(token: &str) -> bool {
    token.chars().any(|c| c.is_alphanumeric())
}

/// 写入侧：切分 → 过滤非词 token → 空格连接。
///
/// 用 `cut_for_search`（索引侧细化）：长词同时产出其子词（`漏斗图` → `漏斗` `漏斗图` `图`），
/// 这样查询侧无论切成粗粒度还是细粒度都能命中；查询侧用 `cut` 保持精度。
pub fn tokenize_for_fts(text: &str) -> String {
    jieba()
        .cut_for_search(text, false)
        .into_iter()
        .map(|token| token.word)
        .filter(|word| is_word(word))
        .collect::<Vec<_>>()
        .join(" ")
}

/// 查询侧：切分 → 过滤非词 token → 去停用词 → 去重（保留首次出现顺序）。
pub fn terms_for_query(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for token in jieba().cut(text, false) {
        let word = token.word.trim();
        if !is_word(word) || is_stopword(word) {
            continue;
        }
        if !out.iter().any(|existing| existing == word) {
            out.push(word.to_string());
        }
    }
    out
}

/// 不做停用词过滤的切分（解析器需要看原文片段，例如「来自 X」）。
pub fn words(text: &str) -> Vec<String> {
    jieba()
        .cut(text, false)
        .into_iter()
        .map(|token| token.word)
        .filter(|word| is_word(word))
        .map(str::to_string)
        .collect()
}

fn is_stopword(word: &str) -> bool {
    let lower = word.to_ascii_lowercase();
    STOPWORDS
        .iter()
        .any(|stop| *stop == word || *stop == lower.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chinese_is_segmented_into_words() {
        let text = "季度营收漏斗图";
        let tokens = tokenize_for_fts(text);
        assert!(!tokens.is_empty());
        assert!(tokens.contains(' '), "token 之间必须有空格：{tokens}");
        // 查询侧（cut）不丢字、不重复：拼回去等于原文
        let query_words: String = words(text).concat();
        assert_eq!(query_words, text, "words={:?}", words(text));
        // 索引侧（cut_for_search）会额外产出子词，但必须包含完整的复合词
        assert!(tokens.contains("营收"), "tokens={tokens}");
        assert!(tokens.contains("漏斗图"), "tokens={tokens}");
    }

    #[test]
    fn punctuation_and_whitespace_are_dropped() {
        let tokens = tokenize_for_fts("导出，\n  PDF！");
        assert!(tokens.contains("导出"));
        assert!(tokens.contains("PDF"));
        assert!(!tokens.contains('，'));
        assert!(!tokens.contains('！'));
    }

    #[test]
    fn query_terms_drop_stopwords_and_dedup() {
        let terms = terms_for_query("我的发票 和 你的发票");
        assert!(terms.contains(&"发票".to_string()));
        assert!(!terms.contains(&"的".to_string()));
        assert_eq!(terms.iter().filter(|t| *t == "发票").count(), 1);
    }

    #[test]
    fn query_terms_and_fts_tokens_share_the_same_words() {
        let text = "数据库连接超时的报错堆栈";
        let indexed = tokenize_for_fts(text);
        for term in terms_for_query(text) {
            assert!(
                indexed.split(' ').any(|word| word == term),
                "查询词 {term} 必须出现在索引文本里：{indexed}"
            );
        }
    }

    #[test]
    fn english_and_mixed_text_is_safe() {
        let terms = terms_for_query("git commit 修复 login 接口");
        assert!(terms.contains(&"git".to_string()));
        assert!(terms.contains(&"login".to_string()));
        assert!(!terms.contains(&"the".to_string()));
    }

    #[test]
    fn empty_input_yields_empty_output() {
        assert!(tokenize_for_fts("").is_empty());
        assert!(terms_for_query("   ").is_empty());
        assert!(terms_for_query("，。！").is_empty());
    }

    #[test]
    fn domain_words_are_single_tokens_in_queries() {
        // 内置领域词的目的是「查询侧不再被切成单字」，因此用 cut（查询侧）验证。
        for word in ["营收", "高亮", "看板", "漏斗图", "饼图", "大盘", "置信度"] {
            let tokens: Vec<&str> = jieba().cut(word, false).iter().map(|t| t.word).collect();
            assert_eq!(tokens, vec![word], "领域词 {word} 不应被切开");
        }
        // 「发票」不能被更长的词遮蔽（否则查「发票」会漏）
        let tokens: Vec<&str> = jieba()
            .cut("发票报销", false)
            .iter()
            .map(|t| t.word)
            .collect();
        assert!(tokens.contains(&"发票"), "tokens={tokens:?}");
    }

    #[test]
    fn index_side_emits_subwords_for_recall() {
        let indexed = tokenize_for_fts("漏斗图");
        // 索引侧细化：既有长词也有子词，任一切分粒度都能命中
        assert!(indexed.contains("漏斗"), "indexed={indexed}");
        assert!(indexed.contains('图'), "indexed={indexed}");
    }
}
