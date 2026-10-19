//! trafilatura-lite 正文提取：从整页 HTML 中剥离导航/广告/页脚/cookie 横幅等噪声，
//! 选出正文子树，产出段落纯文本（供摘要）+ 正文 HTML（供转 Markdown）+ 元数据 + 质量分。
//!
//! 自研而非引入 readability 类依赖：算法可控（CJK 文本密度适配）、零新增重型依赖
//! （复用已有 scraper），且提取失败永远只降级不报错。
//!
//! 实现要点：
//! - **永不失败**：最差返回 quality=0 的空正文，调用方（web_fetch 全页转换 / 搜索退
//!   化 snippet）自行降级
//! - **两阶段**：prune（tag/class/id/hidden 修剪）→ 打分（自底向上聚合子树统计，
//!   倒序遍历保证子先于父，O(n) 单遍）
//! - **显式栈迭代**：html5ever 对嵌套深度无上限，递归遍历会被畸形页面打爆栈
//! - **!Send 纪律**：`scraper::Html` 非 Send，本模块所有入口都是同步纯 CPU 函数，
//!   由调用方放 `spawn_blocking`（见 content/mod.rs 与 mcp/fetch.rs）

use std::collections::HashMap;

use ego_tree::NodeId;
use scraper::{ElementRef, Html, Node, Selector};

use super::text::{is_cjk, normalize_ws};

/// tag 级修剪：非内容元素整体摘除。
const PRUNE_TAGS: &str = "script,style,noscript,template,iframe,svg,canvas,form,button,input,\
                          select,textarea,nav,header,footer,aside,audio,video,object,embed,\
                          dialog,menu";

/// class/id 精确 token 命中即修剪（token 按非字母数字切分，`downloads` 不会命中 `ads`）。
///
/// 注意**不含** `header`/`title`：`class="article-header"` 往往包着标题和首段，误伤代价高；
/// 结构性 header 由 tag 级修剪覆盖。
const PRUNE_CLASS_KEYWORDS: &[&str] = &[
    "cookie",
    "cookies",
    "consent",
    "gdpr",
    "banner",
    "sidebar",
    "sidenav",
    "comment",
    "comments",
    "disqus",
    "share",
    "sharing",
    "social",
    "sociable",
    "ads",
    "ad",
    "advert",
    "adverts",
    "advertisement",
    "advertising",
    "adbox",
    "promo",
    "promotion",
    "popup",
    "popups",
    "modal",
    "overlay",
    "paywall",
    "breadcrumb",
    "breadcrumbs",
    "pagination",
    "pager",
    "related",
    "recommend",
    "recommended",
    "recommendations",
    "widget",
    "widgets",
    "subscribe",
    "subscription",
    "newsletter",
    "signup",
    "register",
    "login",
    "signin",
    "toolbar",
    "masthead",
    "skip",
    "navigation",
    "nav",
    "menu",
    "dropdown",
    "tooltip",
    "notification",
    "notifications",
    "alert",
    "meta",
    "tags",
    "taglist",
    "byline",
    "author",
    "avatar",
    "footer",
];

/// 正文候选 tag。
const CANDIDATE_TAGS: &[&str] = &[
    "article",
    "main",
    "div",
    "section",
    "blockquote",
    "td",
    "pre",
    "li",
    "body",
];

/// 候选子树最小文本量（字符），低于则不参与打分。
const MIN_CANDIDATE_TEXT: usize = 90;

/// 段落收集上限。
const MAX_PARAGRAPHS: usize = 120;

/// 提取产物：正文 + 元数据 + 质量信号。
#[derive(Debug, Clone)]
pub struct ExtractedPage {
    /// 页面标题（og:title > twitter:title > <title>，已剥站点名后缀）。
    pub title: String,
    /// 站点名（og:site_name > JSON-LD publisher.name > host）。
    pub site_name: Option<String>,
    /// 发布日期（YYYY-MM-DD；JSON-LD > meta > <time> > URL 路径）。
    pub date: Option<String>,
    /// 清洗后的段落纯文本（供抽取式摘要）。
    pub paragraphs: Vec<String>,
    /// 正文子树 outerHTML（供 htmd 转 Markdown）。
    pub html: String,
    /// 正文子树总文本量（字符）。
    pub text_len: usize,
    /// 链接文本占比（0~1，越低越像正文）。
    pub link_density: f32,
    /// 综合质量分（0~1）：文本量 + 低链接密度 + 段落数 + 自然语言占比。
    pub quality: f32,
}

/// 提取入口（字符串 HTML）。`pub(crate)`：`mcp/fetch.rs` 直接调用。
pub(crate) fn extract(html: &str, url: &str) -> ExtractedPage {
    let mut doc = Html::parse_document(html);
    // 元数据先于 prune（meta/JSON-LD 多在 head，且不受 tag 修剪保护）
    let (title, site_name, date) = extract_metadata(&doc, url);
    prune(&mut doc);
    build_page(&doc, title, site_name, date)
}

/// 字节入口：先按编码链解码（BOM → Content-Type charset → meta 声明 → UTF-8）再提取。
pub(super) fn extract_bytes(bytes: &[u8], content_type: Option<&str>, url: &str) -> ExtractedPage {
    let html = decode_body(bytes, content_type);
    extract(&html, url)
}

// ── 编码解码 ──────────────────────────────────────────────────────────────

/// 响应体解码。reqwest 的 `text()` 只认 Content-Type 头 charset，中文老站常在
/// meta 里声明 GBK/GB2312 而头里没有——这里显式嗅探（encoding_rs 已是 reqwest
/// 传递依赖）。UTF-8 是最终兜底（decode 对非法序列做替换而非报错）。
fn decode_body(bytes: &[u8], content_type: Option<&str>) -> String {
    let enc = content_type
        .and_then(extract_charset)
        .and_then(|label| encoding_rs::Encoding::for_label(label.as_bytes()))
        .or_else(|| {
            sniff_meta_charset(bytes).and_then(|l| encoding_rs::Encoding::for_label(l.as_bytes()))
        })
        .unwrap_or(encoding_rs::UTF_8);
    let (text, _, _) = enc.decode(bytes);
    text.into_owned()
}

/// `text/html; charset=gbk` → `gbk`。
fn extract_charset(ct: &str) -> Option<String> {
    let lower = ct.to_lowercase();
    let idx = lower.find("charset=")?;
    let rest = &ct[idx + "charset=".len()..];
    let end = rest.find(';').unwrap_or(rest.len());
    let label = rest[..end].trim().trim_matches('"').trim_matches('\'');
    if label.is_empty() {
        None
    } else {
        Some(label.to_string())
    }
}

/// 首 4KB 扫 `<meta charset=...>` / `http-equiv=...content="...charset=..."` 声明。
fn sniff_meta_charset(bytes: &[u8]) -> Option<String> {
    let head = &bytes[..bytes.len().min(4096)];
    // GBK 字节流里 <meta 标签段本身是 ASCII，lossy 足够
    let head = String::from_utf8_lossy(head).to_lowercase();
    for tag_start in head.match_indices("<meta").map(|(i, _)| i) {
        let tag = &head[tag_start..(tag_start + 300).min(head.len())];
        let end = tag.find('>').unwrap_or(tag.len());
        let tag = &tag[..end];
        if let Some(pos) = tag.find("charset=") {
            let mut rest = &tag[pos + "charset=".len()..];
            // `charset="gbk"`：先跳过前导引号，再读到闭合引号/空白/>
            rest = rest.trim_start_matches(['"', '\'']);
            let end = rest
                .find(|c: char| c == '"' || c == '\'' || c.is_whitespace() || c == '>')
                .unwrap_or(rest.len());
            let label = rest[..end].trim();
            if !label.is_empty() {
                return Some(label.to_string());
            }
        }
    }
    None
}

// ── 阶段一：修剪 ──────────────────────────────────────────────────────────

/// tag 选择器 + class/id 关键词 + hidden 三路修剪。
///
/// 全部先收集 `NodeId` 再统一 `detach`（ego-tree 槽位保留、NodeMut::detach 原地解链）。
/// 关键词/hidden 遍历**命中即不向下**（子树整体摘除，避免二次遍历孤儿节点）。
fn prune(doc: &mut Html) {
    let mut remove: Vec<NodeId> = Vec::new();

    if let Ok(sel) = Selector::parse(PRUNE_TAGS) {
        remove.extend(doc.select(&sel).map(|e| e.id()));
    }

    let mut stack = vec![doc.tree.root().id()];
    while let Some(id) = stack.pop() {
        let Some(node) = doc.tree.get(id) else {
            continue;
        };
        if let Node::Element(el) = node.value() {
            if matches_class_keywords(el) || is_hidden(el) {
                remove.push(id);
                continue;
            }
        }
        for child in node.children() {
            stack.push(child.id());
        }
    }

    for id in remove {
        if let Some(mut n) = doc.tree.get_mut(id) {
            n.detach();
        }
    }
}

/// class/id 拼接后按非字母数字切 token，与关键词表**精确匹配**。
fn matches_class_keywords(el: &scraper::node::Element) -> bool {
    let mut hay = String::new();
    if let Some(c) = el.attr("class") {
        hay.push_str(c);
        hay.push(' ');
    }
    if let Some(id) = el.attr("id") {
        hay.push_str(id);
    }
    let hay = hay.to_lowercase();
    hay.split(|c: char| !(c.is_ascii_alphanumeric() || is_cjk(c)))
        .any(|tok| PRUNE_CLASS_KEYWORDS.contains(&tok))
}

/// hidden 属性 / aria-hidden="true" / inline display:none|visibility:hidden。
fn is_hidden(el: &scraper::node::Element) -> bool {
    if el.attr("hidden").is_some() {
        return true;
    }
    if el
        .attr("aria-hidden")
        .is_some_and(|v| v.eq_ignore_ascii_case("true"))
    {
        return true;
    }
    if let Some(style) = el.attr("style") {
        let s: String = style
            .to_lowercase()
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();
        if s.contains("display:none") || s.contains("visibility:hidden") {
            return true;
        }
    }
    false
}

// ── 阶段二：打分选正文 ────────────────────────────────────────────────────

/// 子树统计（自底向上聚合）。
#[derive(Default, Clone, Copy)]
struct Stats {
    text_len: usize,
    link_text_len: usize,
    commas: u32,
    p_count: u32,
}

/// 从修剪后的树中选出正文子树并装配 `ExtractedPage`。
fn build_page(
    doc: &Html,
    title: String,
    site_name: Option<String>,
    date: Option<String>,
) -> ExtractedPage {
    // 1. 预序收集全部节点 id（显式栈；预序保证祖先先于后代）
    let mut order: Vec<NodeId> = Vec::new();
    let mut stack = vec![doc.tree.root().id()];
    while let Some(id) = stack.pop() {
        order.push(id);
        if let Some(node) = doc.tree.get(id) {
            for child in node.children() {
                stack.push(child.id());
            }
        }
    }

    // 2. 倒序遍历聚合子树统计（子先于父）
    let mut stats: HashMap<NodeId, Stats> = HashMap::new();
    for &id in order.iter().rev() {
        let Some(node) = doc.tree.get(id) else {
            continue;
        };
        let mut agg = Stats::default();
        match node.value() {
            Node::Text(t) => {
                let text: &str = t.text.as_ref();
                // 只计非空白字符：<li> 间的缩进换行是独立 Text 节点，计入会虚增
                // text_len 稀释 link_density（链接农场误判为正文）
                agg.text_len = text.chars().filter(|c| !c.is_whitespace()).count();
                agg.commas = text
                    .chars()
                    .filter(|c| [',', ';', '，', '；', '、'].contains(c))
                    .count() as u32;
            }
            Node::Element(el) if el.name() == "p" => {
                agg.p_count = 1;
            }
            _ => {}
        }
        let is_link = matches!(node.value(), Node::Element(el) if el.name() == "a");
        for child in node.children() {
            if let Some(cs) = stats.get(&child.id()) {
                agg.text_len += cs.text_len;
                agg.commas += cs.commas;
                agg.p_count += cs.p_count;
                agg.link_text_len += cs.link_text_len;
                if is_link {
                    // <a> 边界把子树全部文本提升为链接文本
                    agg.link_text_len += cs.text_len - cs.link_text_len;
                }
            }
        }
        stats.insert(id, agg);
    }

    // 3. 候选打分（预序 = 文档序，保证并列时靠前容器胜出）
    let mut best: Option<(f64, NodeId)> = None;
    for &id in &order {
        let Some(node) = doc.tree.get(id) else {
            continue;
        };
        let Node::Element(el) = node.value() else {
            continue;
        };
        if !CANDIDATE_TAGS.contains(&el.name()) {
            continue;
        }
        let Some(s) = stats.get(&id) else { continue };
        if s.text_len < MIN_CANDIDATE_TEXT {
            continue;
        }
        let ld = s.link_text_len as f64 / s.text_len as f64;
        let score = (s.text_len as f64 + 10.0 * s.commas as f64)
            * (1.0 - ld.min(1.0))
            * (1.0 + 0.1 * s.p_count.min(10) as f64)
            * class_bias(el);
        if best.is_none_or(|(b, _)| score > b) {
            best = Some((score, id));
        }
    }

    let Some((_, best_id)) = best else {
        return ExtractedPage {
            title,
            site_name,
            date,
            paragraphs: Vec::new(),
            html: String::new(),
            text_len: 0,
            link_density: 1.0,
            quality: 0.0,
        };
    };

    // 4. 收紧：唯一 >90% 文本的候选 tag 子节点 → 下钻（剥掉包正文的大容器壳）
    let mut current = best_id;
    'tighten: while let Some(total) = stats.get(&current).map(|s| s.text_len) {
        if total == 0 {
            break;
        }
        let Some(node) = doc.tree.get(current) else {
            break;
        };
        for child in node.children() {
            let Some(cs) = stats.get(&child.id()) else {
                continue;
            };
            let is_cand = matches!(
                child.value(),
                Node::Element(el) if CANDIDATE_TAGS.contains(&el.name())
            );
            if is_cand && cs.text_len as f64 > 0.9 * total as f64 {
                current = child.id();
                continue 'tighten;
            }
        }
        break;
    }

    // 5. 装配
    let s = stats.get(&current).copied().unwrap_or_default();
    let link_density = if s.text_len > 0 {
        s.link_text_len as f32 / s.text_len as f32
    } else {
        1.0
    };
    let element = ElementRef::wrap(doc.tree.get(current).expect("node from traversal"))
        .expect("candidate is element");
    let chosen_tag = element.value().name().to_string();

    let mut paragraphs = collect_paragraphs(&element, &chosen_tag);
    paragraphs.truncate(MAX_PARAGRAPHS);

    let joined = paragraphs.join("");
    let p_good = paragraphs
        .iter()
        .filter(|p| p.chars().count() >= 40)
        .count()
        .min(8);
    let quality = (0.4 * (s.text_len as f32 / 1500.0).min(1.0)
        + 0.3 * (1.0 - (link_density * 2.0).min(1.0))
        + 0.2 * (p_good as f32 / 8.0)
        + 0.1 * natural_lang_score(&joined))
    .clamp(0.0, 1.0);

    ExtractedPage {
        title,
        site_name,
        date,
        paragraphs,
        html: element.html(),
        text_len: s.text_len,
        link_density,
        quality,
    }
}

/// class/id 子串偏置：正信号 ×1.2（一次），负信号 ×0.4（一次），可叠加。
///
/// 与 prune 的精确 token 匹配互补：`left-sidebar-col` 这类复合 token 逃过修剪，
/// 但会被这里的子串负偏置压分。
fn class_bias(el: &scraper::node::Element) -> f64 {
    let mut hay = String::new();
    if let Some(c) = el.attr("class") {
        hay.push_str(&c.to_lowercase());
        hay.push(' ');
    }
    if let Some(id) = el.attr("id") {
        hay.push_str(&id.to_lowercase());
    }
    let mut bias = 1.0;
    if [
        "article",
        "content",
        "post",
        "entry",
        "markdown",
        "rich-text",
    ]
    .iter()
    .any(|k| hay.contains(k))
    {
        bias *= 1.2;
    }
    if [
        "sidebar",
        "comment",
        "footer",
        "nav",
        "menu",
        "widget",
        "promo",
        "related",
        "recommend",
        "listing",
    ]
    .iter()
    .any(|k| hay.contains(k))
    {
        bias *= 0.4;
    }
    bias
}

/// 正文子树 → 段落列表（p/pre/blockquote/li 文本，normalize_ws + 去重）。
fn collect_paragraphs(element: &ElementRef, chosen_tag: &str) -> Vec<String> {
    let mut out = Vec::new();
    if ["p", "pre", "blockquote"].contains(&chosen_tag) {
        let t = normalize_ws(&element.text().collect::<String>());
        if t.chars().count() >= 2 {
            out.push(t);
        }
        return out;
    }
    let Ok(sel) = Selector::parse("p, pre, blockquote, li") else {
        return out;
    };
    let mut seen = std::collections::HashSet::new();
    for el in element.select(&sel) {
        let t = normalize_ws(&el.text().collect::<String>());
        let n = t.chars().count();
        if (2..=4000).contains(&n) && seen.insert(t.clone()) {
            out.push(t);
        }
        if out.len() >= MAX_PARAGRAPHS {
            break;
        }
    }
    out
}

/// 自然语言占比信号（quality 的一个小分量）：CJK 占比或英文停用词存在性。
fn natural_lang_score(text: &str) -> f32 {
    if text.is_empty() {
        return 0.0;
    }
    let total = text.chars().filter(|c| !c.is_whitespace()).count();
    if total == 0 {
        return 0.0;
    }
    let cjk = text.chars().filter(|c| is_cjk(*c)).count();
    if cjk > 0 {
        return (cjk as f32 / total as f32).clamp(0.0, 1.0);
    }
    let lower = text.to_lowercase();
    if [" the ", " and ", " is ", " of ", " to "]
        .iter()
        .any(|w| lower.contains(w))
    {
        0.8
    } else {
        0.3
    }
}

// ── 元数据 ────────────────────────────────────────────────────────────────

/// title / site_name / date 三件套。
fn extract_metadata(doc: &Html, url: &str) -> (String, Option<String>, Option<String>) {
    let site_name = meta_content(doc, &["og:site_name"])
        .or_else(|| jsonld_site_name(doc))
        .or_else(|| host_display(url));
    let raw_title = meta_content(doc, &["og:title", "twitter:title"])
        .or_else(|| {
            Selector::parse("title")
                .ok()
                .and_then(|sel| doc.select(&sel).next())
                .map(|t| t.text().collect::<String>())
        })
        .unwrap_or_default();
    let mut title = normalize_ws(&raw_title);
    if let Some(site) = &site_name {
        if !site.is_empty() {
            title = strip_title_site(&title, site);
        }
    }
    let date = extract_date(doc, url);
    (title, site_name, date)
}

/// 依次尝试 `meta[property=X]` / `meta[name=X]` 的 content。
fn meta_content(doc: &Html, props: &[&str]) -> Option<String> {
    for prop in props {
        let selector = format!("meta[property=\"{prop}\"], meta[name=\"{prop}\"]");
        let sel = match Selector::parse(&selector) {
            Ok(s) => s,
            Err(_) => continue,
        };
        if let Some(content) = doc
            .select(&sel)
            .find_map(|m| m.value().attr("content").map(str::to_string))
        {
            let c = normalize_ws(&content);
            if !c.is_empty() {
                return Some(c);
            }
        }
    }
    None
}

/// JSON-LD publisher.name → 站点名。
fn jsonld_site_name(doc: &Html) -> Option<String> {
    let Ok(sel) = Selector::parse("script[type=\"application/ld+json\"]") else {
        return None;
    };
    for script in doc.select(&sel) {
        let text = script.text().collect::<String>();
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
            if let Some(name) = find_jsonld(&v, &["publisher", "name"], 0) {
                return Some(name);
            }
        }
    }
    None
}

/// 剥标题两端的 ` - 站点名` / ` | 站点名` 等后缀（大小写不敏感，字符级安全）。
fn strip_title_site(title: &str, site: &str) -> String {
    if title.is_empty() || site.is_empty() {
        return title.to_string();
    }
    let tl = title.to_lowercase();
    let sl = site.to_lowercase();
    for sep in [" - ", " | ", " _ ", " – ", " — ", "·"] {
        let suffix = format!("{sep}{sl}");
        if tl.ends_with(&suffix) {
            let cut_chars = suffix.chars().count();
            let keep: String = title
                .chars()
                .rev()
                .skip(cut_chars)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            return keep.trim_end().to_string();
        }
        let prefix = format!("{sl}{sep}");
        if tl.starts_with(&prefix) {
            let cut_chars = prefix.chars().count();
            let keep: String = title.chars().skip(cut_chars).collect();
            return keep.trim_start().to_string();
        }
    }
    title.to_string()
}

/// 日期优先级链：JSON-LD → meta → <time> → URL 路径，统一过 normalize_date。
fn extract_date(doc: &Html, url: &str) -> Option<String> {
    let raw = jsonld_date(doc)
        .or_else(|| meta_date(doc))
        .or_else(|| time_date(doc))
        .or_else(|| url_date(url))?;
    normalize_date(&raw)
}

fn jsonld_date(doc: &Html) -> Option<String> {
    let Ok(sel) = Selector::parse("script[type=\"application/ld+json\"]") else {
        return None;
    };
    // 三选一候选字段（非链式），逐个递归查找
    const DATE_KEYS: &[&str] = &["datePublished", "dateModified", "uploadDate"];
    for script in doc.select(&sel) {
        let text = script.text().collect::<String>();
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
            for key in DATE_KEYS {
                if let Some(d) = find_jsonld(&v, &[key], 0) {
                    return Some(d);
                }
            }
        }
    }
    None
}

/// JSON-LD 递归查找（深度上限 6，防恶意深嵌套）。
///
/// 支持两种语义：本层直接命中（`datePublished` → String 值）与链式下探
/// （`publisher` → 对象 → `name`）；`@graph` 等嵌套结构由兜底全值扫描覆盖。
fn find_jsonld(v: &serde_json::Value, chain: &[&str], depth: usize) -> Option<String> {
    if depth > 6 || chain.is_empty() {
        return None;
    }
    match v {
        serde_json::Value::Object(map) => {
            if let Some(serde_json::Value::String(s)) = map.get(chain[0]) {
                return Some(s.clone());
            }
            if let Some(child) = map.get(chain[0]) {
                if let Some(found) = find_jsonld(child, &chain[1..], depth + 1) {
                    return Some(found);
                }
            }
            for val in map.values() {
                if let Some(found) = find_jsonld(val, chain, depth + 1) {
                    return Some(found);
                }
            }
            None
        }
        serde_json::Value::Array(items) => items
            .iter()
            .find_map(|item| find_jsonld(item, chain, depth + 1)),
        _ => None,
    }
}

fn meta_date(doc: &Html) -> Option<String> {
    meta_content(
        doc,
        &[
            "article:published_time",
            "article:modified_time",
            "og:updated_time",
            "pubdate",
            "publishdate",
        ],
    )
}

fn time_date(doc: &Html) -> Option<String> {
    let sel = Selector::parse("time[datetime]").ok()?;
    doc.select(&sel)
        .find_map(|t| t.value().attr("datetime").map(str::to_string))
}

/// URL 路径日期：`/2026/03/12/` 三连段或 `/2026-03-12-` 单段。
fn url_date(url: &str) -> Option<String> {
    let parsed = url::Url::parse(url).ok()?;
    let segments: Vec<&str> = parsed.path_segments()?.collect();
    for win in segments.windows(3) {
        if let (Some(y), Some(m), Some(d)) = (num4(win[0]), num12(win[1]), num12(win[2])) {
            return Some(format!("{y:04}-{m:02}-{d:02}"));
        }
    }
    for seg in &segments {
        let digits: Vec<&str> = seg.split('-').collect();
        if digits.len() >= 3 {
            if let (Some(y), Some(m), Some(d)) =
                (num4(digits[0]), num12(digits[1]), num12(digits[2]))
            {
                return Some(format!("{y:04}-{m:02}-{d:02}"));
            }
        }
    }
    None
}

fn num4(s: &str) -> Option<u32> {
    if s.len() == 4 && s.chars().all(|c| c.is_ascii_digit()) {
        s.parse().ok()
    } else {
        None
    }
}

fn num12(s: &str) -> Option<u32> {
    let n = s.chars().count();
    if (1..=2).contains(&n) && s.chars().all(|c| c.is_ascii_digit()) {
        s.parse().ok().filter(|v| (1..=31).contains(v))
    } else {
        None
    }
}

/// 多格式日期归一化到 `YYYY-MM-DD`；合理性窗口 1995-01-01 .. 今天+2 天。
fn normalize_date(raw: &str) -> Option<String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    let day = chrono::DateTime::parse_from_rfc3339(raw)
        .map(|dt| dt.date_naive())
        .ok()
        .or_else(|| {
            let head: String = raw.chars().take(10).collect();
            chrono::NaiveDate::parse_from_str(&head, "%Y-%m-%d").ok()
        })
        .or_else(|| chrono::NaiveDate::parse_from_str(raw, "%Y-%m-%d").ok())
        .or_else(|| chrono::NaiveDate::parse_from_str(raw, "%Y/%m/%d").ok())
        .or_else(|| chrono::NaiveDate::parse_from_str(raw, "%Y年%m月%d日").ok())
        .or_else(|| chrono::NaiveDate::parse_from_str(raw, "%m/%d/%Y").ok())?;
    let lo = chrono::NaiveDate::from_ymd_opt(1995, 1, 1)?;
    let hi = chrono::Utc::now().date_naive() + chrono::Duration::days(2);
    if day < lo || day > hi {
        return None;
    }
    Some(day.format("%Y-%m-%d").to_string())
}

/// host 作为 site_name 兜底（去端口，保留原样大小写的 host_str 已是小写）。
fn host_display(url: &str) -> Option<String> {
    let parsed = url::Url::parse(url).ok()?;
    let host = parsed.host_str()?;
    let host = host.trim_start_matches("www.");
    if host.is_empty() {
        None
    } else {
        Some(host.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ARTICLE_HTML: &str = r#"
    <html><head>
        <title>Tokio JoinSet 用法指南 - Example Blog</title>
        <meta property="og:site_name" content="Example Blog"/>
        <meta property="article:published_time" content="2026-03-12T08:00:00+00:00"/>
    </head><body>
        <nav><a href="/">首页</a> <a href="/docs">文档</a> <a href="/about">关于</a></nav>
        <div class="cookie-banner">我们使用 cookie<a id="consent-ok">同意</a></div>
        <div id="main-content">
            <article>
                <h1>Tokio JoinSet 用法指南</h1>
                <p>JoinSet 是 tokio 提供的并发任务管理器。它允许你同时 spawn 多个任务并统一收集结果。当任意任务完成时，join_next 会立即返回。</p>
                <p>使用 JoinSet 管理并发请求时，可以配合 timeout 实现整体超时控制。超时后 JoinSet drop 会自动 abort 残留任务，不会泄漏。</p>
                <pre><code>let mut set = JoinSet::new();</code></pre>
                <p>总体而言，JoinSet 适合扇出场景。它内部维护任务句柄，完成顺序即 join_next 返回顺序，与 spawn 顺序无关。</p>
            </article>
            <div class="sidebar"><a href="/a">相关文章一</a><a href="/b">相关文章二</a><a href="/c">相关文章三</a></div>
        </div>
        <footer><p>© 2026 Example Blog. All rights reserved.</p></footer>
    </body></html>
    "#;

    #[test]
    fn test_extract_article_main_content() {
        let page = extract(ARTICLE_HTML, "https://example.blog/posts/tokio-joinset");
        // 标题剥站点名后缀
        assert!(
            page.title.contains("Tokio JoinSet"),
            "title: {}",
            page.title
        );
        assert!(
            !page.title.contains("Example Blog"),
            "title: {}",
            page.title
        );
        assert_eq!(page.site_name.as_deref(), Some("Example Blog"));
        assert_eq!(page.date.as_deref(), Some("2026-03-12"));
        // 正文选中 article：段落包含核心内容
        let joined = page.paragraphs.join("\n");
        assert!(
            joined.contains("JoinSet 是 tokio 提供的并发任务管理器"),
            "joined: {joined}"
        );
        // 导航/cookie/页脚被剪掉
        assert!(!joined.contains("首页"), "nav leaked: {joined}");
        assert!(!joined.contains("cookie"), "cookie banner leaked: {joined}");
        assert!(
            !joined.contains("All rights reserved"),
            "footer leaked: {joined}"
        );
        // 正文 html 不含导航
        assert!(!page.html.contains("cookie-banner"));
        assert!(page.quality > 0.35, "quality: {}", page.quality);
        assert!(page.link_density < 0.3);
    }

    #[test]
    fn test_extract_link_farm_low_quality() {
        let farm = r#"
        <html><body><div id="content">
            <ul>
                <li><a href="/1">免费下载 Rust 教程第一份资料点这里</a></li>
                <li><a href="/2">免费下载 Rust 教程第二份资料点这里</a></li>
                <li><a href="/3">免费下载 Rust 教程第三份资料点这里</a></li>
                <li><a href="/4">免费下载 Rust 教程第四份资料点这里</a></li>
                <li><a href="/5">免费下载 Rust 教程第五份资料点这里</a></li>
                <li><a href="/6">免费下载 Rust 教程第六份资料点这里</a></li>
            </ul>
        </div></body></html>"#;
        let page = extract(farm, "https://farm.example.com/list");
        assert!(page.link_density > 0.6, "farm ld: {}", page.link_density);
    }

    #[test]
    fn test_extract_empty_shell() {
        let page = extract(
            "<html><body><div id=\"root\"></div></body></html>",
            "https://spa.example.com/",
        );
        assert_eq!(page.quality, 0.0);
        assert!(page.paragraphs.is_empty());
        assert!(page.html.is_empty());
    }

    #[test]
    fn test_jsonld_date_nested() {
        let html = r#"
        <html><head><script type="application/ld+json">
        {"@context":"https://schema.org","@graph":[{"@type":"NewsArticle","datePublished":"2026-01-15T00:00:00Z","headline":"x"}]}
        </script></head><body><p>内容内容内容内容内容内容内容内容内容内容内容内容内容内容内容内容内容内容内容内容内容</p></body></html>"#;
        let page = extract(html, "https://example.com/news");
        assert_eq!(page.date.as_deref(), Some("2026-01-15"));
    }

    #[test]
    fn test_time_and_url_date() {
        let html = r#"<html><body><article><p>正文</p><time datetime="2025-12-01T10:00:00+08:00">12月1日</time></article></body></html>"#;
        let page = extract(html, "https://example.com/");
        assert_eq!(page.date.as_deref(), Some("2025-12-01"));
        // 无页面声明时退到 URL 路径
        let page2 = extract(
            "<html><body><p>内容</p></body></html>",
            "https://example.com/2026/03/12/story/",
        );
        assert_eq!(page2.date.as_deref(), Some("2026-03-12"));
        let page3 = extract(
            "<html><body><p>内容</p></body></html>",
            "https://example.com/2026-03-12-story/",
        );
        assert_eq!(page3.date.as_deref(), Some("2026-03-12"));
    }

    #[test]
    fn test_normalize_date_window_and_formats() {
        assert_eq!(
            normalize_date("2026-03-12T08:00:00+00:00").as_deref(),
            Some("2026-03-12")
        );
        assert_eq!(normalize_date("2026/03/12").as_deref(), Some("2026-03-12"));
        assert_eq!(
            normalize_date("2026年3月12日").as_deref(),
            Some("2026-03-12")
        );
        assert_eq!(normalize_date("03/12/2026").as_deref(), Some("2026-03-12"));
        // 远古 / 未来超出合理性窗口
        assert_eq!(normalize_date("1900-01-01"), None);
        assert_eq!(normalize_date("2035-06-01"), None);
        assert_eq!(normalize_date("garbage"), None);
    }

    #[test]
    fn test_decode_body_gbk() {
        let (bytes, _, _) = encoding_rs::GBK.encode("<html><head><meta charset=\"gbk\"></head><body><p>中文内容测试，验证解码。</p></body></html>");
        let html = decode_body(&bytes, Some("text/html"));
        assert!(html.contains("中文内容测试"), "decoded: {html}");
    }

    #[test]
    fn test_decode_body_content_type_charset() {
        let (bytes, _, _) =
            encoding_rs::GBK.encode("<html><body><p>编码链优先级</p></body></html>");
        let html = decode_body(&bytes, Some("text/html; charset=GBK"));
        assert!(html.contains("编码链优先级"));
    }

    #[test]
    fn test_sniff_meta_charset() {
        let (bytes, _, _) = encoding_rs::GBK.encode("<html><head><meta http-equiv=\"Content-Type\" content=\"text/html; charset=gb2312\"></head><body></body></html>");
        assert_eq!(sniff_meta_charset(&bytes).as_deref(), Some("gb2312"));
    }

    #[test]
    fn test_strip_title_site() {
        assert_eq!(
            strip_title_site("指南 - Example Blog", "example blog"),
            "指南"
        );
        assert_eq!(
            strip_title_site("Example Blog | 指南", "Example Blog"),
            "指南"
        );
        assert_eq!(strip_title_site("无站点后缀", "Site"), "无站点后缀");
    }

    #[test]
    fn test_matches_class_keywords_token_exact() {
        let doc = Html::parse_document("<div class=\"downloads\">x</div>");
        let el = doc.select(&Selector::parse("div").unwrap()).next().unwrap();
        assert!(
            !matches_class_keywords(el.value()),
            "downloads 不应命中 ads"
        );
        let doc2 = Html::parse_document("<div class=\"sidebar-ads\">x</div>");
        let el2 = doc2
            .select(&Selector::parse("div").unwrap())
            .next()
            .unwrap();
        assert!(matches_class_keywords(el2.value()), "sidebar-ads 命中 ads");
    }

    #[test]
    fn test_big_page_no_stack_overflow() {
        // 1 万层深嵌套：显式栈遍历必须不爆栈
        let mut html = String::from("<html><body>");
        html.push_str(&"<div>".repeat(10_000));
        html.push_str(&"</div>".repeat(10_000));
        html.push_str("</body></html>");
        let page = extract(&html, "https://example.com/deep");
        // 不 panic 即通过（正文可能为空）
        assert!(page.quality >= 0.0);
    }
}
