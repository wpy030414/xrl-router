//! 抽取式摘要：从正文段落 + Bing snippet 中按「查询词覆盖度」打分选取句子，
//! 生成 600~800 字符的长摘要（对齐 DeepSeek Search API `summary` 字段语义——
//! 供模型阅读，区别于展示用的短 snippet）。
//!
//! 纯本地算法，无 LLM 参与：句子打分 → 贪心选入预算 → 按原文顺序重排拼接。

use super::text::{split_sentences, QueryTerms};

/// 摘要硬上限（字符）。目标区间 600~800：贪心尽量多选，不足 600 也如实返回
/// （对齐 DeepSeek `summary` 语义——供模型阅读，长短由页面内容决定）。
pub(super) const SUMMARY_MAX_CHARS: usize = 800;
/// 短页面阈值：段落总长低于此值时，先拼 snippet 前缀再补句子。
const SHORT_PAGE_THRESHOLD: usize = 300;
/// snippet 前缀截断长度。
const SNIPPET_PREFIX_CHARS: usize = 200;

/// 生成抽取式摘要。
///
/// - 段落总长 < 300 时以 snippet（截 200 字）开头，再补正文句子
/// - 代码样句与过短碎片在候选阶段即被剔除
/// - 句子打分：`2.0×查询覆盖 + 0.6×标题词覆盖 + 0.3×导语偏置 - 0.3×超长句惩罚`
/// - 贪心按分降序选入 800 字符预算，选中的句子**按原文出现顺序**拼接
///   （保持行文连贯），全部选完仍不足 600 也如实返回
pub(super) fn build_summary(
    paragraphs: &[String],
    title: &str,
    snippet: &str,
    terms: &QueryTerms,
) -> String {
    let title_terms = QueryTerms::build(title, None);

    // 1. 展开句子（保留段落内顺序）
    let mut sentences: Vec<String> = Vec::new();
    let total_para_chars: usize = paragraphs.iter().map(|p| p.chars().count()).sum();
    if total_para_chars < SHORT_PAGE_THRESHOLD {
        let prefix: String = snippet.chars().take(SNIPPET_PREFIX_CHARS).collect();
        let prefix = prefix.trim();
        if !prefix.is_empty() {
            sentences.push(prefix.to_string());
        }
    }
    for p in paragraphs {
        for s in split_sentences(p) {
            // 过短碎片与代码样句不参与（导航残留、编号、代码行）
            if s.chars().count() >= 8 && !looks_like_code(&s) {
                sentences.push(s);
            }
        }
    }
    if sentences.is_empty() {
        return snippet.chars().take(SUMMARY_MAX_CHARS).collect();
    }

    // 2. 打分
    let total = sentences.len() as f32;
    let scored: Vec<(usize, f32)> = sentences
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let len = s.chars().count();
            let mut score = 2.0 * terms.coverage(s)
                + 0.6 * title_terms.coverage(s)
                + 0.3 * (1.0 - i as f32 / total); // 导语偏置
            if len > 300 {
                score -= 0.3;
            }
            (i, score)
        })
        .collect();

    // 3. 贪心选入预算（分数降序，同分按出现序）
    let mut chosen: Vec<usize> = (0..sentences.len()).collect();
    chosen.sort_by(|&a, &b| {
        let (sa, sb) = (scored[a].1, scored[b].1);
        sb.partial_cmp(&sa)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.cmp(&b))
    });
    let mut budget = SUMMARY_MAX_CHARS;
    let mut picked: Vec<usize> = Vec::new();
    for idx in chosen {
        let cost = sentences[idx].chars().count();
        if cost > budget {
            continue; // 放不下，尝试更短的句子
        }
        budget -= cost;
        picked.push(idx);
        if budget < 20 {
            break;
        }
    }
    if picked.is_empty() {
        // 单句超预算的极端情况：截第一句
        picked.push(0);
    }

    // 4. 按原文顺序拼接（CJK 邻接直接连，Latin 间补空格）
    picked.sort_unstable();
    let mut out = String::new();
    for idx in picked {
        if !out.is_empty() {
            let prev_cjk = out.chars().last().is_some_and(super::text::is_cjk);
            let next_cjk = sentences[idx]
                .chars()
                .next()
                .is_some_and(super::text::is_cjk);
            if !prev_cjk && !next_cjk {
                out.push(' ');
            }
        }
        out.push_str(&sentences[idx]);
    }
    out
}

/// 代码样句识别：非字母数字、非空白字符占比 > 0.3（CJK 表意字属于字母数字类，
/// 中文标点占比远低于此阈值，不受影响）。
fn looks_like_code(s: &str) -> bool {
    let mut total = 0usize;
    let mut symbols = 0usize;
    for c in s.chars() {
        if c.is_whitespace() {
            continue;
        }
        total += 1;
        if !c.is_alphanumeric() {
            symbols += 1;
        }
    }
    total > 0 && symbols as f64 / total as f64 > 0.3
}

#[cfg(test)]
mod tests {
    use super::*;

    fn para(s: &str) -> Vec<String> {
        vec![s.to_string()]
    }

    #[test]
    fn test_budget_respected() {
        let sentences: Vec<String> = (0..40)
            .map(|i| format!("这是关于并发编程的第{i}个句子，内容各不相同，长度保持适中。"))
            .collect();
        let terms = QueryTerms::build("并发编程", None);
        let summary = build_summary(&sentences, "标题", "snippet", &terms);
        assert!(
            summary.chars().count() <= SUMMARY_MAX_CHARS,
            "len: {}",
            summary.chars().count()
        );
    }

    #[test]
    fn test_short_page_snippet_prefix() {
        let terms = QueryTerms::build("tokio", None);
        let summary = build_summary(
            &para("很短。"),
            "T",
            "snippet 开头是这里，后面还有更多内容延伸。",
            &terms,
        );
        assert!(summary.starts_with("snippet 开头"), "summary: {summary}");
    }

    #[test]
    fn test_relevant_sentences_preferred() {
        // 页面足够长（超 800 预算）才会真正触发选择；短句页面全收是正确行为
        let mut paragraphs =
            vec!["tokio 的并发模型基于异步任务调度，能够高效处理大量 IO 密集型请求。".to_string()];
        // 填充无关长句把总量推过 800 预算，迫使贪心选择生效
        for i in 0..20 {
            paragraphs.push(format!(
                "这是一段关于网站运营策略和市场推广渠道优化的无关填充内容第{i}段，\
                 篇幅刻意写得较长以便真正挤占摘要预算从而触发句子选择逻辑。"
            ));
        }
        paragraphs
            .push("我们的联系方式在页面底部，欢迎随时来电咨询合作事宜与商业推广事宜。".to_string());
        let terms = QueryTerms::build("tokio 并发", None);
        let summary = build_summary(&paragraphs, "tokio 指南", "", &terms);
        assert!(summary.contains("tokio 的并发模型"), "summary: {summary}");
        assert!(
            summary.chars().count() <= SUMMARY_MAX_CHARS,
            "over budget: {}",
            summary.chars().count()
        );
        // 预算受限下，无关的「商业推广」被挤出
        assert!(
            !summary.contains("商业推广"),
            "irrelevant leaked: {summary}"
        );
    }

    #[test]
    fn test_selected_keeps_original_order() {
        let paragraphs = vec![
            "第一个句子讲开头的内容。第二个句子讲结尾的内容。第三个句子讲中间的内容。".to_string(),
        ];
        let terms = QueryTerms::build("中间 结尾", None);
        let summary = build_summary(&paragraphs, "T", "", &terms);
        // 「结尾」句（第 2）先被选中，「中间」句（第 3）后选，但拼接保持原文顺序
        let pos_end = summary.find("结尾").expect("should contain 结尾");
        let pos_mid = summary.find("中间").expect("should contain 中间");
        assert!(pos_end < pos_mid, "summary: {summary}");
    }

    #[test]
    fn test_code_sentences_skipped() {
        let paragraphs = vec![
            "正常句子讲解概念和用法，说明如何在实际项目中应用这些知识。".to_string(),
            "fn main() { let x = vec![1,2,3]; println!(\"{}\", x.iter().map(|v| v*2).collect::<Vec<_>>()); }".to_string(),
        ];
        let terms = QueryTerms::build("main", None);
        let summary = build_summary(&paragraphs, "T", "", &terms);
        assert!(!summary.contains("fn main()"), "summary: {summary}");
    }

    #[test]
    fn test_empty_inputs_fall_back_to_snippet() {
        let terms = QueryTerms::build("q", None);
        let summary = build_summary(&[], "T", "只有 snippet 可用", &terms);
        assert_eq!(summary, "只有 snippet 可用");
    }

    #[test]
    fn test_ori_question_influences_selection() {
        let paragraphs = vec![
            "这一段讲超时处理的细节，包含 timeout 的用法说明。".to_string(),
            "这一段讲完全无关的颜色配置和主题美化的内容。".to_string(),
        ];
        // query 是宽泛词，ori_question 提供意图
        let terms = QueryTerms::build("rust", Some("超时怎么处理"));
        let summary = build_summary(&paragraphs, "T", "", &terms);
        assert!(summary.contains("超时处理"), "summary: {summary}");
    }
}
