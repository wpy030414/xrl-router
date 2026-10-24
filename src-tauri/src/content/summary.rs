//! 命中段摘录：围绕与查询最相关的句子取**连续窗口**，而不是把整页句子按分数
//! 贪心挑选再拼接。
//!
//! 为什么改（ADR-057）：旧实现把 5000 字正文压成 800 字「高频句合集」，句子
//! 之间没有上下文衔接，表格/代码/数字大多落在窗口之外——对模型而言是高噪声
//! token。社区做法（Exa highlights、Tavily 清洗片段）都是**定位命中段落并给出
//! 上下文**，让模型判断值不值得 `web_fetch` 全文。
//!
//! 纯本地算法，无 LLM 参与：句子打分 → 取最高分句 → 向前后扩展连续窗口。

use super::text::{split_sentences, QueryTerms};

/// 摘录硬上限（字符）。窗口扩展到此为止。
pub(super) const EXCERPT_MAX_CHARS: usize = 600;
/// 短页面阈值：段落总长低于此值时，先拼 snippet 前缀再补窗口。
const SHORT_PAGE_THRESHOLD: usize = 300;
/// snippet 前缀截断长度。
const SNIPPET_PREFIX_CHARS: usize = 200;

/// 生成命中段摘录。
///
/// - 句子打分：`2.0×查询覆盖 + 0.6×标题词覆盖 + 0.3×导语偏置 - 0.3×超长句惩罚`
/// - 取最高分句为锚点，向前后交替扩展（最多各 2 句、总量 ≤ 600 字符）
/// - 段落总长 < 300 时先拼 snippet（截 200 字）前缀
/// - 无可用句子时退化为 snippet（截 600 字）
pub(super) fn build_excerpt(
    paragraphs: &[String],
    title: &str,
    snippet: &str,
    terms: &QueryTerms,
) -> String {
    let title_terms = QueryTerms::build(title, None);

    let total_para_chars: usize = paragraphs.iter().map(|p| p.chars().count()).sum();
    let prefix = if total_para_chars < SHORT_PAGE_THRESHOLD {
        let text: String = snippet.chars().take(SNIPPET_PREFIX_CHARS).collect();
        let text = text.trim();
        if text.is_empty() {
            None
        } else {
            Some(text.to_string())
        }
    } else {
        None
    };

    // 1. 展开句子（保留原文顺序；过短碎片与代码样句剔除）
    let mut sentences: Vec<String> = Vec::new();
    for paragraph in paragraphs {
        for sentence in split_sentences(paragraph) {
            if sentence.chars().count() >= 8 && !looks_like_code(&sentence) {
                sentences.push(sentence);
            }
        }
    }
    if sentences.is_empty() {
        return prefix.unwrap_or_else(|| snippet.chars().take(EXCERPT_MAX_CHARS).collect());
    }

    // 2. 打分
    let total = sentences.len() as f32;
    let mut best = 0usize;
    let mut best_score = f32::MIN;
    for (i, sentence) in sentences.iter().enumerate() {
        let len = sentence.chars().count();
        let mut score = 2.0 * terms.coverage(sentence)
            + 0.6 * title_terms.coverage(sentence)
            + 0.3 * (1.0 - i as f32 / total); // 导语偏置
        if len > 300 {
            score -= 0.3;
        }
        if score > best_score {
            best_score = score;
            best = i;
        }
    }

    // 3. 连续窗口扩展：前后交替，受字符预算与单侧句数双重约束
    let mut start = best;
    let mut end = best;
    let mut used = sentences[best].chars().count();
    let mut grew = true;
    while grew {
        grew = false;
        if end + 1 < sentences.len() && end - start < 4 {
            let cost = sentences[end + 1].chars().count();
            if used + cost <= EXCERPT_MAX_CHARS {
                end += 1;
                used += cost;
                grew = true;
            }
        }
        if start > 0 {
            let cost = sentences[start - 1].chars().count();
            if used + cost <= EXCERPT_MAX_CHARS {
                start -= 1;
                used += cost;
                grew = true;
            }
        }
    }

    // 4. 拼接（CJK 邻接直接连，Latin 之间补空格）
    let mut out = String::new();
    if let Some(prefix) = prefix {
        out.push_str(&prefix);
    }
    for sentence in &sentences[start..=end] {
        if !out.is_empty() {
            let prev_cjk = out.chars().last().is_some_and(super::text::is_cjk);
            let next_cjk = sentence.chars().next().is_some_and(super::text::is_cjk);
            if !prev_cjk && !next_cjk {
                out.push(' ');
            }
        }
        out.push_str(sentence);
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

    fn paras(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn test_budget_respected() {
        let paragraphs: Vec<String> = (0..40)
            .map(|i| format!("这是关于并发编程的第{i}个句子，内容各不相同，长度保持适中。"))
            .collect();
        let terms = QueryTerms::build("并发编程", None);
        let excerpt = build_excerpt(&paragraphs, "标题", "snippet", &terms);
        assert!(
            excerpt.chars().count() <= EXCERPT_MAX_CHARS,
            "len: {}",
            excerpt.chars().count()
        );
    }

    #[test]
    fn test_short_page_snippet_prefix() {
        let terms = QueryTerms::build("tokio", None);
        let excerpt = build_excerpt(
            &paras(&["很短。"]),
            "T",
            "snippet 开头是这里，后面还有更多内容延伸。",
            &terms,
        );
        assert!(excerpt.starts_with("snippet 开头"), "excerpt: {excerpt}");
    }

    #[test]
    fn test_excerpt_is_contiguous_around_best_sentence() {
        // 每句约 200 字符：600 预算最多容纳 3 句，窗口必须停在锚点附近
        let filler = |tag: &str| format!("{tag}{}", "无关内容填充".repeat(30));
        let paragraphs = paras(&[
            &filler("第一段讲市场推广："),
            &filler("第二段讲运营策略："),
            &filler("第三段讲完全无关的美化："),
            "第四段讲解 tokio 的并发模型：基于异步任务调度，能够高效处理大量 IO 密集型请求，             并通过工作窃取在多个线程间均衡负载，适合高并发网络服务。",
            &filler("第五段讲 JoinSet 的用法："),
            &filler("第六段讲商业合作："),
        ]);
        let terms = QueryTerms::build("tokio 并发", None);
        let excerpt = build_excerpt(&paragraphs, "指南", "", &terms);
        // 锚点句及其相邻句进入窗口
        assert!(excerpt.contains("tokio 的并发模型"), "excerpt: {excerpt}");
        assert!(excerpt.contains("JoinSet"), "excerpt: {excerpt}");
        assert!(excerpt.contains("第三段"), "excerpt: {excerpt}");
        // 远端无关内容不进入窗口
        assert!(!excerpt.contains("商业合作"), "excerpt: {excerpt}");
        assert!(!excerpt.contains("市场推广"), "excerpt: {excerpt}");
    }

    #[test]
    fn test_empty_inputs_fall_back_to_snippet() {
        let terms = QueryTerms::build("q", None);
        let excerpt = build_excerpt(&[], "T", "只有 snippet 可用", &terms);
        assert_eq!(excerpt, "只有 snippet 可用");
    }

    #[test]
    fn test_code_sentences_skipped() {
        let paragraphs = paras(&[
            "正常句子讲解概念和用法，说明如何在实际项目中应用这些知识。",
            "fn main() { let x = vec![1,2,3]; println!(\"{}\", x.iter().map(|v| v*2).collect::<Vec<_>>()); }",
        ]);
        let terms = QueryTerms::build("main", None);
        let excerpt = build_excerpt(&paragraphs, "T", "", &terms);
        assert!(!excerpt.contains("fn main()"), "excerpt: {excerpt}");
    }

    #[test]
    fn test_ori_question_influences_selection() {
        let paragraphs = paras(&[
            "这一段讲超时处理的细节，包含 timeout 的用法说明。",
            "这一段讲完全无关的颜色配置和主题美化的内容。",
        ]);
        let terms = QueryTerms::build("rust", Some("超时怎么处理"));
        let excerpt = build_excerpt(&paragraphs, "T", "", &terms);
        assert!(excerpt.contains("超时处理"), "excerpt: {excerpt}");
    }
}
