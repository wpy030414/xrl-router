//! 文本处理基元：分词（CJK bigram + Latin 分词）、分句、查询词项覆盖度。
//!
//! summary（句子打分选取）与 rank（结果相关性打分）共用同一套词项逻辑，
//! 独立成模块避免 rank 反向依赖 summary。
//!
//! 分词策略（面向"查询词项覆盖度"而非语义理解）：
//! - CJK 连续段：滑窗出 bigram（`张雪峰` → `张雪`/`雪峰`，跨界 bigram `峰高` 是
//!   可接受的噪声——覆盖度只关心命中与否）；段长为 1 时保留单字；先剔单字虚词
//! - Latin/数字段：非字母数字边界切词、小写化、滤停用词与单字符词

/// 查询词项总数上限：防超长 ori_question 稀释主查询权重。
const MAX_TERMS: usize = 24;

/// Latin 停用词（查询侧剔除；正文侧不使用）。
const LATIN_STOPWORDS: &[&str] = &[
    "the", "a", "an", "is", "are", "was", "were", "of", "to", "in", "on", "for", "and", "or",
    "how", "what", "when", "where", "which", "who", "whom", "why", "do", "does", "did", "with",
    "by", "from", "at", "as", "be", "been", "being", "it", "its", "this", "that", "these", "those",
    "can", "could", "will", "would", "shall", "should", "may", "might", "must", "i", "you", "we",
    "they", "he", "she", "it", "my", "your", "our", "their", "me", "us", "them", "there", "here",
    "about", "into", "over", "under", "than", "then", "so", "such", "not", "no", "nor", "too",
    "very", "just", "use", "using", "used",
];

/// CJK 单字虚词：bigram 切分前剔除，避免「的」「了」之类高频虚词污染覆盖度。
const CJK_STOPCHARS: &[char] = &[
    '的', '了', '是', '在', '和', '与', '或', '也', '就', '都', '还', '又', '再', '被', '把', '给',
    '对', '从', '向', '以', '至', '于', '因', '为', '所', '以', '但', '如', '果', '不', '过', '而',
    '然', '此', '么', '呢', '吧', '啊', '吗', '嘛', '么',
];

/// 主 CJK 区段判断（含扩展 A / 兼容区 / 假名；标点不属于此判定）。
pub(super) fn is_cjk(c: char) -> bool {
    matches!(c,
        '\u{4E00}'..='\u{9FFF}'      // CJK 统一表意
        | '\u{3400}'..='\u{4DBF}'    // 扩展 A
        | '\u{F900}'..='\u{FAFF}'    // 兼容表意
        | '\u{3040}'..='\u{30FF}'    // 平假名/片假名
    )
}

/// 分词：CJK 段滑窗 bigram + Latin 段小写分词（滤停用词/单字符）。
///
/// 混排文本一次扫描同时产出两类词项，保持出现顺序（重复项由 `QueryTerms::build` 合并）。
pub(super) fn tokenize(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut cjk_run: Vec<char> = Vec::new();
    let mut latin_run = String::new();

    fn flush_cjk(run: &mut Vec<char>, out: &mut Vec<String>) {
        if run.len() == 1 {
            out.push(run[0].to_string());
        } else if run.len() >= 2 {
            for pair in run.windows(2) {
                out.push(pair.iter().collect());
            }
        }
        run.clear();
    }
    fn flush_latin(run: &mut String, out: &mut Vec<String>) {
        if run.chars().count() >= 2 {
            let w = run.to_lowercase();
            if !LATIN_STOPWORDS.contains(&w.as_str()) {
                out.push(w);
            }
        }
        run.clear();
    }

    for c in text.chars() {
        if is_cjk(c) {
            flush_latin(&mut latin_run, &mut out);
            if CJK_STOPCHARS.contains(&c) {
                continue;
            }
            cjk_run.push(c);
        } else if c.is_alphanumeric() {
            flush_cjk(&mut cjk_run, &mut out);
            latin_run.push(c);
        } else {
            flush_cjk(&mut cjk_run, &mut out);
            flush_latin(&mut latin_run, &mut out);
        }
    }
    flush_cjk(&mut cjk_run, &mut out);
    flush_latin(&mut latin_run, &mut out);
    out
}

/// 查询词项集合：term → 权重（query 1.0 / ori_question 0.5，重复取最大权重）。
pub(super) struct QueryTerms {
    terms: Vec<(String, f32)>,
    total_weight: f32,
}

impl QueryTerms {
    /// 合并 query 与可选 ori_question 的词项。
    ///
    /// ori_question 是多轮对话的原始意图（对齐 DeepSeek Search API 的同名参数语义），
    /// 权重 0.5——引导重排与摘要选取，但不淹没当前 query。
    pub fn build(query: &str, ori_question: Option<&str>) -> Self {
        let mut merged: Vec<(String, f32)> = Vec::new();
        for (text, weight) in [(query, 1.0f32)]
            .into_iter()
            .chain(ori_question.into_iter().map(|q| (q, 0.5f32)))
        {
            for term in tokenize(text) {
                match merged.iter_mut().find(|(t, _)| *t == term) {
                    Some((_, w)) => *w = (*w).max(weight),
                    None => merged.push((term, weight)),
                }
            }
        }
        // 权重降序 + 出现序稳定排序，截断到 MAX_TERMS
        let orig = merged;
        let mut indexed: Vec<(usize, (String, f32))> = orig.into_iter().enumerate().collect();
        indexed.sort_by(|a, b| {
            b.1 .1
                .partial_cmp(&a.1 .1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.0.cmp(&b.0))
        });
        indexed.truncate(MAX_TERMS);
        let terms: Vec<(String, f32)> = indexed.into_iter().map(|(_, t)| t).collect();
        let total_weight: f32 = terms.iter().map(|(_, w)| w).sum();
        Self {
            terms,
            total_weight,
        }
    }

    /// 覆盖度 0.0~1.0：命中词项权重和 / 总权重。
    ///
    /// 正文**不预分词**：CJK bigram 直接子串判定；Latin 词走词边界（先按非字母数字
    /// 切出词集合再查成员，`rust` 不会命中 `frustrating`）。
    pub fn coverage(&self, text: &str) -> f32 {
        if self.terms.is_empty() || self.total_weight <= 0.0 || text.is_empty() {
            return 0.0;
        }
        let lowered = text.to_lowercase();
        let words: std::collections::HashSet<&str> = lowered
            .split(|c: char| !c.is_ascii_alphanumeric())
            .filter(|w| w.chars().count() >= 2)
            .collect();
        let hit: f32 = self
            .terms
            .iter()
            .filter(|(term, _)| {
                if term.chars().any(is_cjk) {
                    lowered.contains(term.as_str())
                } else {
                    words.contains(term.as_str())
                }
            })
            .map(|(_, w)| w)
            .sum();
        (hit / self.total_weight).clamp(0.0, 1.0)
    }
}

/// 分句：段落 → 句子列表。
///
/// - CJK 终止符 `。！？；` 与 Latin `!?` 直接切
/// - `.` 仅在后随（跳过空白）大写/引号/CJK 或文末时切——保护 `Node.js`、`3.5`
/// - `…` 不是终止符；`，、` 句内不切
/// - 返回 trim 后非空句子
pub(super) fn split_sentences(paragraph: &str) -> Vec<String> {
    let chars: Vec<char> = paragraph.chars().collect();
    let mut out = Vec::new();
    let mut start = 0usize;
    let mut i = 0usize;
    while i < chars.len() {
        let c = chars[i];
        let cut = match c {
            // 全角终止符：中文语境无歧义，直接切
            '。' | '！' | '？' | '；' => true,
            // ASCII ! ? . ：要求后随空白再判下一字符，避免 vec![..] / println!(..) /
            // Node.js / 3.5 这类代码与小数被误切
            '!' | '?' | '.' => ascii_ends_sentence(&chars, i),
            _ => false,
        };
        if cut {
            let sentence: String = chars[start..=i].iter().collect();
            let trimmed = sentence.trim();
            if !trimmed.is_empty() {
                out.push(trimmed.to_string());
            }
            start = i + 1;
        }
        i += 1;
    }
    let tail: String = chars[start..].iter().collect();
    let trimmed = tail.trim();
    if !trimmed.is_empty() {
        out.push(trimmed.to_string());
    }
    out
}

/// ASCII `! ? .` 是否结束句子：跳过连续终止符后，须位于文末或后随空白，
/// 且空白后是大写字母/引号/开括号/CJK。避免 `vec![` `println!(` `Node.js`
/// `3.5` 这类代码与小数被误切。
fn ascii_ends_sentence(chars: &[char], i: usize) -> bool {
    let mut j = i + 1;
    // 跳过连续终止符（"What?!" / "end.." 视为一个终止点）
    while j < chars.len() && matches!(chars[j], '!' | '?' | '.') {
        j += 1;
    }
    if j >= chars.len() {
        return true; // 文末
    }
    // 后随必须先有空白才可能是句界
    if !chars[j].is_whitespace() {
        return false;
    }
    let mut k = j;
    while k < chars.len() && chars[k].is_whitespace() {
        k += 1;
    }
    match chars.get(k) {
        None => true,
        Some(&next) => next.is_ascii_uppercase() || "\"'“‘（([{《".contains(next) || is_cjk(next),
    }
}

/// 空白折叠 + 去零宽字符（U+200B~U+200F / U+FEFF 常见于复制粘贴的网页文本）。
pub(super) fn normalize_ws(s: &str) -> String {
    s.chars()
        .filter(|c| !matches!(c, '\u{200B}'..='\u{200F}' | '\u{FEFF}'))
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tokenize_cjk_bigrams() {
        // 张雪峰高考 → 4 个 bigram（含跨界 bigram，属可接受噪声）
        assert_eq!(tokenize("张雪峰高考"), vec!["张雪", "雪峰", "峰高", "高考"]);
        // 单字段保留单字
        assert_eq!(tokenize("涨"), vec!["涨"]);
    }

    #[test]
    fn test_tokenize_stopchars_removed() {
        // 「的」「了」虚词剔除后再滑窗：雪峰 → bigram 雪峰
        assert_eq!(tokenize("雪的峰"), vec!["雪峰"]);
        assert!(tokenize("的了").is_empty());
    }

    #[test]
    fn test_tokenize_mixed() {
        let terms = tokenize("Rust 的 tokio JoinSet 用法");
        assert!(terms.contains(&"rust".to_string()));
        assert!(terms.contains(&"tokio".to_string()));
        assert!(terms.contains(&"joinset".to_string()));
        assert!(terms.contains(&"用法".to_string()));
    }

    #[test]
    fn test_tokenize_latin_stopwords() {
        assert!(tokenize("how to the a is of").is_empty());
        assert_eq!(tokenize("JoinSet-based"), vec!["joinset", "based"]);
    }

    #[test]
    fn test_coverage_latin_word_boundary() {
        let terms = QueryTerms::build("rust", None);
        // "rust" 不命中 "frustrating"
        assert_eq!(terms.coverage("frustrating experience"), 0.0);
        assert_eq!(terms.coverage("rust-lang is great"), 1.0);
        assert_eq!(terms.coverage("Rust 语法"), 1.0);
    }

    #[test]
    fn test_coverage_cjk_substring() {
        let terms = QueryTerms::build("tokio 并发", None);
        assert_eq!(terms.coverage("tokio 的并发模型"), 1.0);
        assert_eq!(terms.coverage("无关文本"), 0.0);
        assert_eq!(terms.coverage("只有 tokio"), 0.5);
    }

    #[test]
    fn test_query_terms_weight_merge() {
        // query 与 ori_question 重合词项取最大权重（1.0），不叠加成 1.5
        let terms = QueryTerms::build("rust 并发", Some("怎么在 rust 里做并发"));
        let rust = terms.terms.iter().find(|(t, _)| t == "rust").unwrap();
        assert_eq!(rust.1, 1.0);
    }

    #[test]
    fn test_query_terms_ori_question_only_boosts() {
        // ori_question 独有词项权重 0.5
        let terms = QueryTerms::build("tokio", Some("超时怎么处理"));
        let t = terms
            .terms
            .iter()
            .find(|(term, _)| term.contains("超时"))
            .unwrap();
        assert_eq!(t.1, 0.5);
    }

    #[test]
    fn test_split_sentences_cjk() {
        let s = split_sentences("第一句。第二句！第三句？");
        assert_eq!(s, vec!["第一句。", "第二句！", "第三句？"]);
    }

    #[test]
    fn test_split_sentences_decimal_and_dot_names() {
        let s = split_sentences("Node.js 3.5 is fast. Next sentence here.");
        assert_eq!(s.len(), 2);
        assert!(s[0].contains("Node.js 3.5"));
        assert!(s[1].starts_with("Next"));
    }

    #[test]
    fn test_split_sentences_dot_before_cjk() {
        let s = split_sentences("abc. 你好。结尾");
        assert_eq!(s, vec!["abc.", "你好。", "结尾"]);
    }

    #[test]
    fn test_split_sentences_ellipsis_not_terminator() {
        let s = split_sentences("等等…还没结束。");
        assert_eq!(s, vec!["等等…还没结束。"]);
    }

    #[test]
    fn test_normalize_ws() {
        // U+200B 零宽空格删除后 a/b 相连；其余空白折叠为单空格
        assert_eq!(normalize_ws("a\u{200B}b   c\n\td"), "ab c d");
        assert_eq!(normalize_ws("  spaced  "), "spaced");
    }

    #[test]
    fn test_query_terms_max_cap() {
        let long_query = (0..40)
            .map(|i| format!("word{i}"))
            .collect::<Vec<_>>()
            .join(" ");
        let terms = QueryTerms::build(&long_query, None);
        assert!(terms.terms.len() <= 24);
    }
}
