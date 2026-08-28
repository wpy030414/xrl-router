//! HuggingFace 客户端：搜索 / 仓库信息 / 文件列表 / 下载（支持镜像）。
//!
//! 基础 URL：settings 表 `hf_mirror` = "1" 时用 `https://hf-mirror.com`，
//! 否则 `https://huggingface.co`。
//! 契约见 docs/specs/spec-local-models.md。

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use futures::StreamExt;
use serde::{Deserialize, Serialize};
use tracing::warn;

pub const HF_BASE: &str = "https://huggingface.co";
pub const HF_MIRROR: &str = "https://hf-mirror.com";

/// 搜索结果的仓库摘要。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HfRepoSummary {
    pub id: String,
    pub downloads: i64,
    pub likes: i64,
    pub tags: Vec<String>,
    pub description: Option<String>,
}

/// 仓库文件条目（tree API）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HfFile {
    pub path: String,
    pub size: Option<i64>,
}

/// 仓库详情（文件列表 + 元信息）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HfRepoDetail {
    pub id: String,
    pub downloads: i64,
    pub likes: i64,
    pub tags: Vec<String>,
    pub files: Vec<HfFile>,
}

pub struct HfClient {
    base: String,
    client: reqwest::Client,
}

impl HfClient {
    pub fn new(base: String, client: reqwest::Client) -> Self {
        Self { base, client }
    }

    pub fn base_from_setting(mirror: bool) -> String {
        if mirror {
            HF_MIRROR.to_string()
        } else {
            HF_BASE.to_string()
        }
    }

    /// 搜索模型仓库（filter=gguf）。
    pub async fn search(
        &self,
        q: &str,
        filter: &str,
        limit: u32,
    ) -> anyhow::Result<Vec<HfRepoSummary>> {
        let url = format!(
            "{}/api/models?search={}&limit={}&filter={}",
            self.base,
            urlencoding(q),
            limit,
            filter
        );
        let resp = self.client.get(&url).send().await?;
        if !resp.status().is_success() {
            return Err(anyhow::anyhow!("HF search failed: HTTP {}", resp.status()));
        }
        let arr: Vec<serde_json::Value> = resp.json().await?;
        Ok(arr
            .into_iter()
            .filter_map(|v| {
                let id = v.get("id")?.as_str()?.to_string();
                Some(HfRepoSummary {
                    id,
                    downloads: v.get("downloads").and_then(|d| d.as_i64()).unwrap_or(0),
                    likes: v.get("likes").and_then(|d| d.as_i64()).unwrap_or(0),
                    tags: v
                        .get("tags")
                        .and_then(|t| t.as_array())
                        .map(|t| {
                            t.iter()
                                .filter_map(|x| x.as_str().map(String::from))
                                .collect()
                        })
                        .unwrap_or_default(),
                    description: v
                        .get("description")
                        .and_then(|d| d.as_str())
                        .map(String::from),
                })
            })
            .collect())
    }

    /// 仓库详情：siblings 文件列表 + tree API 文件大小。
    pub async fn repo_detail(&self, repo: &str) -> anyhow::Result<HfRepoDetail> {
        let info_url = format!("{}/api/models/{}", self.base, repo);
        let info_resp = self.client.get(&info_url).send().await?;
        if !info_resp.status().is_success() {
            return Err(anyhow::anyhow!(
                "HF repo info failed: HTTP {}",
                info_resp.status()
            ));
        }
        let info: serde_json::Value = info_resp.json().await?;
        let siblings: Vec<String> = info
            .get("siblings")
            .and_then(|s| s.as_array())
            .map(|s| {
                s.iter()
                    .filter_map(|x| x.get("rfilename").and_then(|f| f.as_str()).map(String::from))
                    .collect()
            })
            .unwrap_or_default();

        let tree_url = format!(
            "{}/api/models/{}/tree/main?recursive=true",
            self.base, repo
        );
        let mut files: Vec<HfFile> = Vec::new();
        if let Ok(resp) = self.client.get(&tree_url).send().await {
            if resp.status().is_success() {
                if let Ok(arr) = resp.json::<Vec<serde_json::Value>>().await {
                    for v in arr {
                        if let (Some(path), Some(ty)) = (
                            v.get("path").and_then(|p| p.as_str()),
                            v.get("type").and_then(|t| t.as_str()),
                        ) {
                            if ty == "file" {
                                files.push(HfFile {
                                    path: path.to_string(),
                                    size: v.get("size").and_then(|s| s.as_i64()),
                                });
                            }
                        }
                    }
                }
            } else {
                warn!(
                    repo,
                    status = %resp.status(),
                    "HF tree API failed, sizes unavailable"
                );
            }
        }
        if files.is_empty() {
            files = siblings
                .into_iter()
                .map(|path| HfFile { path, size: None })
                .collect();
        }

        Ok(HfRepoDetail {
            id: info
                .get("id")
                .and_then(|i| i.as_str())
                .unwrap_or(repo)
                .to_string(),
            downloads: info.get("downloads").and_then(|d| d.as_i64()).unwrap_or(0),
            likes: info.get("likes").and_then(|d| d.as_i64()).unwrap_or(0),
            tags: info
                .get("tags")
                .and_then(|t| t.as_array())
                .map(|t| {
                    t.iter()
                        .filter_map(|x| x.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default(),
            files,
        })
    }

    /// 下载权重文件（Range 断点续传 + 进度回调 + 取消）。
    pub async fn download(
        &self,
        repo: &str,
        file: &str,
        dest: &Path,
        total: Option<u64>,
        cancel: &AtomicBool,
        mut on_progress: impl FnMut(u64, u64),
    ) -> anyhow::Result<()> {
        let part = dest.with_extension("part");
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let existing = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);

        let url = format!("{}/{}/resolve/main/{}", self.base, repo, file);
        let mut req = self.client.get(&url);
        if existing > 0 {
            req = req.header("Range", format!("bytes={}-", existing));
        }
        let resp = req.send().await?;
        let status = resp.status();
        let mut expected = total.or_else(|| resp.content_length());
        if !status.is_success() {
            return Err(anyhow::anyhow!(
                "HF download failed: HTTP {} ({})",
                status,
                url
            ));
        }
        if existing > 0 && status != reqwest::StatusCode::PARTIAL_CONTENT {
            expected = resp.content_length().or(total);
        }

        let mut file_handle = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&part)?;
        let mut stream = resp.bytes_stream();
        let mut downloaded = existing;
        let mut last_emit = Instant::now();
        while let Some(chunk) = stream.next().await {
            if cancel.load(Ordering::Relaxed) {
                return Err(anyhow::anyhow!("download cancelled"));
            }
            let chunk = chunk?;
            std::io::Write::write_all(&mut file_handle, &chunk)?;
            downloaded += chunk.len() as u64;
            if last_emit.elapsed() >= Duration::from_millis(200) {
                on_progress(downloaded, expected.unwrap_or(0));
                last_emit = Instant::now();
            }
        }
        std::io::Write::flush(&mut file_handle)?;

        if let Some(t) = expected {
            if downloaded != t {
                return Err(anyhow::anyhow!(
                    "size mismatch: expected {} bytes, got {}",
                    t,
                    downloaded
                ));
            }
        }
        on_progress(downloaded, expected.unwrap_or(downloaded));
        std::fs::rename(&part, dest)?;
        Ok(())
    }
}

fn urlencoding(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' | '.' | '~' => c.to_string(),
            _ => format!("%{:02X}", c as u32),
        })
        .collect()
}
