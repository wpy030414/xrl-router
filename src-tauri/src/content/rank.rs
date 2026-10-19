//! rerank_score：0.0~3.0 的语义重排分（对齐 DeepSeek Search API 的同名参数语义，
//! ≥1.5 视为相关）。
//!
//! 纯启发式组合，四个分量上限合计恰为 3.0：
//! - 相关度 ≤1.2：查询词项覆盖（title 0.55 / snippet 0.30 / body 0.15 加权）
//! - 权威度 ≤0.8：域名分级线性映射（-1→0，1→0.32，4→0.8）
//! - 新鲜度 ≤0.5：发布日期距今天数分档
//! - 内容质量 ≤0.5：readability 质量分（未抓到正文为 0）
//!
//! 外加 0.02 以内的原始名次先验（并列打破，稳定排序保持 Bing 原序）。

use chrono::{DateTime, Utc};

use super::text::QueryTerms;

/// 打分输入（借用聚合，避免克隆大字符串）。
pub(super) struct ScoreParts<'a> {
    pub title: &'a str,
    pub snippet: &'a str,
    /// 抓到的正文全文（None 时退化为 snippet 参与覆盖计算）。
    pub body_text: Option<&'a str>,
    pub authority: i8,
    pub date: Option<&'a str>,
    /// readability 质量分（未抓到为 None）。
    pub quality: Option<f32>,
    /// 原始名次先验：(1 - 名次/候选数)，调用方计算。
    pub order_bias: f32,
}

/// 计算 rerank_score，结果 clamp 到 [0, 3]。
pub(super) fn rerank_score(p: &ScoreParts, terms: &QueryTerms, now: DateTime<Utc>) -> f32 {
    let body = p.body_text.unwrap_or(p.snippet);
    let relevance = 1.2
        * (0.55 * terms.coverage(p.title)
            + 0.30 * terms.coverage(p.snippet)
            + 0.15 * terms.coverage(body));
    let authority = 0.8 * (p.authority as f32 + 1.0) / 5.0;
    let freshness = freshness_bonus(p.date, now);
    let quality = 0.5 * p.quality.unwrap_or(0.0);
    (relevance + authority + freshness + quality + 0.02 * p.order_bias.clamp(0.0, 1.0))
        .clamp(0.0, 3.0)
}

/// 新鲜度分档：≤7 天 0.5 / ≤30 天 0.4 / ≤180 天 0.25 / ≤365 天 0.15 / 更早、未来或未知 0。
pub(super) fn freshness_bonus(date: Option<&str>, now: DateTime<Utc>) -> f32 {
    let Some(d) = date else { return 0.0 };
    let Ok(day) = chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d") else {
        return 0.0;
    };
    let days = (now.date_naive() - day).num_days();
    match days {
        0..=7 => 0.5,
        8..=30 => 0.4,
        31..=180 => 0.25,
        181..=365 => 0.15,
        _ => 0.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> DateTime<Utc> {
        chrono::Utc::now()
    }

    fn parts<'a>(
        title: &'a str,
        snippet: &'a str,
        body: Option<&'a str>,
        authority: i8,
        date: Option<&'a str>,
        quality: Option<f32>,
    ) -> ScoreParts<'a> {
        ScoreParts {
            title,
            snippet,
            body_text: body,
            authority,
            date,
            quality,
            order_bias: 0.5,
        }
    }

    #[test]
    fn test_full_relevant_recent_authoritative() {
        let terms = QueryTerms::build("tokio JoinSet 超时", None);
        let p = parts(
            "tokio JoinSet 超时控制指南",
            "讲解 JoinSet 与 timeout 的配合用法",
            Some("正文详述 tokio JoinSet 超时控制……"),
            4,
            Some("2026-10-15"),
            Some(0.9),
        );
        let score = rerank_score(&p, &terms, now());
        // 相关度满分 1.2 + 权威 0.8 + 新鲜 0.5 + 质量 0.45 + 名次 0.01 ≈ 2.96
        assert!(score > 2.5, "score: {score}");
        assert!(score <= 3.0);
    }

    #[test]
    fn test_irrelevant_low_authority_old() {
        let terms = QueryTerms::build("tokio 并发", None);
        let p = parts(
            "美食菜谱大全",
            "今天吃什么好",
            Some(" cooking recipes for dinner tonight with vegetables "),
            1,
            None,
            None,
        );
        let score = rerank_score(&p, &terms, now());
        // 相关度 0 + 权威 0.32 + 新鲜 0 + 质量 0 + 名次 0.01
        assert!(score < 0.5, "score: {score}");
    }

    #[test]
    fn test_title_weighs_more_than_body() {
        let terms = QueryTerms::build("rust", None);
        let title_hit = parts("rust 入门", "无关摘要", None, 1, None, None);
        let body_hit = parts(
            "无关标题",
            "无关摘要",
            Some("rust 出现在正文深处"),
            1,
            None,
            None,
        );
        assert!(rerank_score(&title_hit, &terms, now()) > rerank_score(&body_hit, &terms, now()));
    }

    #[test]
    fn test_authority_mapping() {
        let terms = QueryTerms::build("x", None);
        let mut p = parts("t", "s", None, -1, None, None);
        let a_neg1 = rerank_score(&p, &terms, now());
        p.authority = 4;
        let a_4 = rerank_score(&p, &terms, now());
        p.authority = 1;
        let a_1 = rerank_score(&p, &terms, now());
        assert!(a_neg1 < a_1 && a_1 < a_4, "{a_neg1} {a_1} {a_4}");
        // -1 → 0：与 quality/date 全零且覆盖全零时总分仅剩名次先验
        let p2 = parts("t", "s", None, -1, None, None);
        assert!(rerank_score(&p2, &terms, now()) <= 0.02 + 1e-6);
    }

    #[test]
    fn test_freshness_tiers() {
        let n = now();
        let today = n.date_naive().format("%Y-%m-%d").to_string();
        let d30 = (n - chrono::Duration::days(20))
            .date_naive()
            .format("%Y-%m-%d")
            .to_string();
        let d200 = (n - chrono::Duration::days(200))
            .date_naive()
            .format("%Y-%m-%d")
            .to_string();
        let future = (n + chrono::Duration::days(30))
            .date_naive()
            .format("%Y-%m-%d")
            .to_string();
        assert_eq!(freshness_bonus(Some(&today), n), 0.5);
        assert_eq!(freshness_bonus(Some(&d30), n), 0.4);
        assert_eq!(freshness_bonus(Some(&d200), n), 0.15);
        assert_eq!(freshness_bonus(Some(&future), n), 0.0);
        assert_eq!(freshness_bonus(None, n), 0.0);
        assert_eq!(freshness_bonus(Some("garbage"), n), 0.0);
    }

    #[test]
    fn test_score_clamped_to_3() {
        let terms = QueryTerms::build("a b", None);
        let p = parts("a b", "a b", Some("a b"), 4, Some("2026-10-18"), Some(1.0));
        assert!(rerank_score(&p, &terms, now()) <= 3.0);
    }
}
