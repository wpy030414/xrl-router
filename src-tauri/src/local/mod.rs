//! 本地模型管理（私有化）：下载 → 引擎启动 → provider 注册 → 生命周期。
//!
//! 契约见 docs/specs/spec-local-models.md。要点：
//! - 事件经 `AppState.key_stats_tx` 广播：`local_progress` / `local_status`。
//! - 对外暴露：每台本地模型注册一个 Chat Completions provider（id = `local-{model_id}`）
//!   + 一条模型（display_name = 用户命名）+ 一条随机密钥（AES 加密入库、同步进 KeyPool）。
//! - 引擎：llama-server（GGUF）。
//! - 崩溃自动重启：120s 启动健康检查；退出后按 5s/15s/45s 退避重启，最多 3 次。

pub mod backend;
pub mod engine;
pub mod hf;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Deserialize;
use tracing::{error, info, warn};

use crate::crypto::MasterKey;
use crate::db::Database;
use crate::keys::KeyPool;
use crate::providers::ProviderRegistry;
use crate::types::{ApiKey, LocalModel, Model, Provider, ProviderKind};

use backend::{Backend, BackendDetect};
use hf::{HfClient, HfRepoDetail, HfRepoSummary};

/// 引擎健康检查超时。llama-server 加载完模型才起 HTTP 服务，大模型（如 27B Q8_0）
/// 在 CPU 上加载可能 1-2 分钟，120s 覆盖常规场景，超时后引导看引擎日志。
const START_TIMEOUT: Duration = Duration::from_secs(120);
/// 崩溃重启退避序列。
const RESTART_BACKOFFS: [Duration; 3] = [
    Duration::from_secs(5),
    Duration::from_secs(15),
    Duration::from_secs(45),
];

/// 创建本地模型的请求体。
/// `local_path` 非空时走「导入本地权重」：跳过下载，直接拷贝已有文件入库。
#[derive(Debug, Clone, Deserialize)]
pub struct CreateLocalModelReq {
    pub repo_id: String,
    pub filename: String,
    pub format: String,
    pub backend: String,
    pub model_id: String,
    pub ctx_size: i64,
    pub n_gpu_layers: i64,
    pub autostart: bool,
    #[serde(default)]
    pub local_path: Option<String>,
}

/// 编辑本地模型参数的请求体（仅更新非 None 字段）。
#[derive(Debug, Clone, Deserialize)]
pub struct EditLocalModelReq {
    pub model_id: Option<String>,
    pub ctx_size: Option<i64>,
    pub n_gpu_layers: Option<i64>,
    pub backend: Option<String>,
    pub autostart: Option<bool>,
    pub thinking: Option<bool>,
}

/// 运行中的引擎句柄。
struct RunningEngine {
    child: Option<tokio::process::Child>,
    /// 引擎进程 PID（用于 stop() 跨线程杀进程，避免与 watcher 抢 Child 句柄）。
    pid: u32,
}

#[derive(Clone)]
pub struct LocalManager {
    db: Database,
    master_key: MasterKey,
    models_dir: PathBuf,
    engines_dir: PathBuf,
    http_client: reqwest::Client,
    providers: ProviderRegistry,
    keys: KeyPool,
    events_tx: tokio::sync::broadcast::Sender<serde_json::Value>,
    /// model id → 运行中引擎。
    running: Arc<Mutex<HashMap<String, RunningEngine>>>,
    /// model id → 下载取消标志。
    downloads: Arc<Mutex<HashMap<String, Arc<AtomicBool>>>>,
}

impl LocalManager {
    pub fn new(
        db: Database,
        master_key: MasterKey,
        data_dir: &std::path::Path,
        http_client: reqwest::Client,
        providers: ProviderRegistry,
        keys: KeyPool,
        events_tx: tokio::sync::broadcast::Sender<serde_json::Value>,
    ) -> Self {
        let models_dir = data_dir.join("models");
        let engines_dir = data_dir.join("engines");
        let _ = std::fs::create_dir_all(&models_dir);
        let _ = std::fs::create_dir_all(&engines_dir);
        Self {
            db,
            master_key,
            models_dir,
            engines_dir,
            http_client,
            providers,
            keys,
            events_tx,
            running: Arc::new(Mutex::new(HashMap::new())),
            downloads: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    // ---------- HF 只读 ----------

    fn hf_client(&self) -> HfClient {
        let mirror = self
            .db
            .get_setting("hf_mirror")
            .ok()
            .flatten()
            .map(|v| v == "true" || v == "1")
            .unwrap_or(false);
        HfClient::new(HfClient::base_from_setting(mirror), self.http_client.clone())
    }

    pub async fn hf_search(
        &self,
        q: &str,
        filter: &str,
        limit: u32,
    ) -> anyhow::Result<Vec<HfRepoSummary>> {
        self.hf_client().search(q, filter, limit).await
    }

    pub async fn hf_repo(&self, repo: &str) -> anyhow::Result<HfRepoDetail> {
        self.hf_client().repo_detail(repo).await
    }

    /// 文件大小查询（下载前校验目标大小）。
    pub async fn hf_file_size(&self, repo: &str, file: &str) -> anyhow::Result<Option<i64>> {
        let detail = self.hf_client().repo_detail(repo).await?;
        Ok(detail
            .files
            .iter()
            .find(|f| f.path == file)
            .and_then(|f| f.size))
    }

    // ---------- 查询 ----------

    pub fn backends(&self) -> BackendDetect {
        backend::detect()
    }

    /// 当前是否启用 HF 镜像（前端展示）。
    pub fn hf_mirror_enabled(&self) -> bool {
        self.db
            .get_setting("hf_mirror")
            .ok()
            .flatten()
            .map(|v| v == "true" || v == "1")
            .unwrap_or(false)
    }

    /// 切换 HF 镜像开关（持久化到 settings）。
    pub fn set_hf_mirror(&self, on: bool) -> Result<(), String> {
        self.db
            .set_setting("hf_mirror", if on { "1" } else { "0" })
            .map_err(|e| e.to_string())
    }

    pub fn list(&self) -> anyhow::Result<Vec<LocalModel>> {
        self.db.list_local_models()
    }

    // ---------- 下载 ----------

    /// 创建本地模型：校验撞名 → 落库（downloading → 后台下载 /
    /// 导入本地权重 → 拷贝文件后直接 downloaded）。
    pub async fn create(&self, req: CreateLocalModelReq) -> Result<LocalModel, String> {
        if self.db.alias_taken(&req.model_id) {
            return Err(format!("模型名 '{}' 已被占用", req.model_id));
        }
        if req.format != "gguf" {
            return Err(format!("不支持的格式: {}", req.format));
        }

        // 导入路径：文件必须存在且为 .gguf
        let import_src = req
            .local_path
            .as_deref()
            .filter(|p| !p.is_empty())
            .map(std::path::PathBuf::from);
        if let Some(src) = &import_src {
            if !src.is_file() {
                return Err(format!("文件不存在: {}", src.display()));
            }
            if src.extension().map(|e| e != "gguf").unwrap_or(true) {
                return Err("仅支持导入 .gguf 格式的权重文件".to_string());
            }
        }

        let id = format!("lm-{}", uuid::Uuid::new_v4().simple().to_string());
        let now = chrono::Utc::now().timestamp();
        let mut m = LocalModel {
            id: id.clone(),
            repo_id: req.repo_id.clone(),
            filename: req.filename.clone(),
            format: req.format.clone(),
            backend: resolve_backend(&req.backend, &req.format),
            status: if import_src.is_some() { "downloaded" } else { "downloading" }.to_string(),
            model_id: req.model_id.clone(),
            ctx_size: req.ctx_size.max(1024),
            n_gpu_layers: req.n_gpu_layers.max(0),
            autostart: if req.autostart { 1 } else { 0 },
            // 新建默认开启思考（V21 DB 默认值语义）；开关在编辑页调整
            thinking: 1,
            file_size: None,
            local_path: self.model_file_path(&id, &req.format).to_string_lossy().to_string(),
            port: None,
            created_at: now,
            updated_at: now,
        };

        match &import_src {
            Some(src) => {
                // 本地导入：拷贝到应用数据目录，文件元数据取大小（拷贝中途失败则回滚）
                let dest = self.model_file_path(&id, &req.format);
                if let Some(parent) = dest.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                if let Err(e) = std::fs::copy(src, &dest) {
                    return Err(format!("拷贝权重文件失败: {}", e));
                }
                match std::fs::metadata(&dest) {
                    Ok(md) => m.file_size = Some(md.len() as i64),
                    Err(_) => {
                        let _ = std::fs::remove_file(&dest);
                        return Err("无法读取导入文件的元数据".to_string());
                    }
                }
                info!(id = %m.id, src = %src.display(), "imported local weights");
            }
            None => {
                // 目标大小：tree API 查一次（失败不阻断，下载时用 Content-Length）
                match self.hf_file_size(&req.repo_id, &req.filename).await {
                    Ok(Some(size)) => m.file_size = Some(size),
                    Ok(None) => warn!(repo = %req.repo_id, file = %req.filename, "file not found in HF tree"),
                    Err(e) => warn!(error = %e, "hf tree fetch failed, size unknown"),
                }
            }
        }

        self.db.save_local_model(&m).map_err(|e| e.to_string())?;
        if import_src.is_none() {
            self.spawn_download(m.clone());
        } else {
            self.broadcast_status(&m, None);
        }
        Ok(m)
    }

    /// 编辑本地模型参数（ctx_size / n_gpu_layers / backend / autostart / thinking）。
    /// 引擎运行中修改需下次启动生效。
    pub fn edit(&self, id: &str, req: EditLocalModelReq) -> anyhow::Result<LocalModel> {
        let mut m = self
            .db
            .get_local_model(id)?
            .ok_or_else(|| anyhow::anyhow!("模型不存在: {}", id))?;
        if let Some(ref new_name) = req.model_id {
            if new_name != &m.model_id && self.db.alias_taken(new_name) {
                return Err(anyhow::anyhow!("模型名 '{}' 已被占用", new_name));
            }
        }
        if let Some(v) = req.ctx_size {
            m.ctx_size = v.max(1024);
        }
        if let Some(v) = req.n_gpu_layers {
            m.n_gpu_layers = v.max(0);
        }
        if let Some(b) = req.backend {
            m.backend = b;
        }
        if let Some(a) = req.autostart {
            m.autostart = if a { 1 } else { 0 };
        }
        if let Some(t) = req.thinking {
            m.thinking = if t { 1 } else { 0 };
        }
        if let Some(new_name) = req.model_id {
            if new_name != m.model_id {
                let old_name = m.model_id.clone();
                m.model_id = new_name.clone();
                // 同步更新 provider 名和 model display_name
                let provider_id = format!("local-{}", m.id);
                if let Some(mut p) = self.providers.get(&provider_id) {
                    p.name = format!("本地 · {}", new_name);
                    p.updated_at = chrono::Utc::now().timestamp();
                    let _ = self.db.save_provider(&p);
                    self.providers.insert(p);
                }
                let model_db_id = format!("local-m-{}", m.id);
                if let Some(mut model) = self.db.get_model(&model_db_id).ok().flatten() {
                    model.display_name = new_name;
                    model.updated_at = chrono::Utc::now().timestamp();
                    let _ = self.db.save_model(&model);
                }
                info!(id = %m.id, "model renamed: {} → {}", old_name, m.model_id);
            }
        }
        m.updated_at = chrono::Utc::now().timestamp();
        self.db.save_local_model(&m)?;
        self.broadcast_status(&m, None);
        Ok(m)
    }

    fn model_file_path(&self, id: &str, _format: &str) -> PathBuf {
        // GGUF only：权重文件统一放在 models/{id}/model.gguf
        self.models_dir.join(id).join("model.gguf")
    }

    fn spawn_download(&self, m: LocalModel) {
        let cancel = Arc::new(AtomicBool::new(false));
        {
            let mut map = self.downloads.lock().unwrap();
            map.insert(m.id.clone(), cancel.clone());
        }
        let this = self.clone();
        info!(id = %m.id, repo = %m.repo_id, file = %m.filename, "download started");
        tokio::spawn(async move {
            let result = this.run_download(&m, cancel.clone()).await;
            this.downloads.lock().unwrap().remove(&m.id);
            match result {
                Ok(()) => {
                    let mut m2 = m.clone();
                    m2.status = "downloaded".to_string();
                    m2.updated_at = chrono::Utc::now().timestamp();
                    let _ = this.db.save_local_model(&m2);
                    this.broadcast_status(&m2, None);
                }
                Err(e) => {
                    let msg = e.to_string();
                    let mut m2 = m.clone();
                    m2.status = "error".to_string();
                    m2.updated_at = chrono::Utc::now().timestamp();
                    let _ = this.db.save_local_model(&m2);
                    this.broadcast_status(&m2, Some(msg));
                    let _ = std::fs::remove_file(
                        this.model_file_path(&m.id, &m.format).with_extension("part"),
                    );
                }
            }
        });
    }

    async fn run_download(
        &self,
        m: &LocalModel,
        cancel: Arc<AtomicBool>,
    ) -> anyhow::Result<()> {
        let dest = self.model_file_path(&m.id, &m.format);
        self.hf_client()
            .download(
                &m.repo_id,
                &m.filename,
                &dest,
                m.file_size.map(|s| s as u64),
                &cancel,
                |d, t| {
                    let _ = self.events_tx.send(serde_json::json!({
                        "type": "local_progress",
                        "id": m.id,
                        "downloaded": d,
                        "total": t,
                    }));
                },
            )
            .await?;
        Ok(())
    }

    pub fn cancel(&self, id: &str) -> Result<(), String> {
        let flag = self.downloads.lock().unwrap().get(id).cloned();
        match flag {
            Some(f) => {
                f.store(true, Ordering::Relaxed);
                Ok(())
            }
            None => Err("没有进行中的下载".to_string()),
        }
    }

    // ---------- 引擎生命周期 ----------

    /// 启动：resolve 后端 → 确保引擎 → spawn → 健康检查 → 注册 provider。
    pub async fn start(&self, id: &str) -> Result<(), String> {
        let m = self
            .db
            .get_local_model(id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "本地模型不存在".to_string())?;
        if m.status == "downloading" {
            return Err("权重仍在下载中，请稍候".to_string());
        }
        if self.running.lock().unwrap().contains_key(id) {
            return Ok(());
        }
        let port = engine::free_port().map_err(|e| e.to_string())?;
        let api_key = random_api_key();
        let backends = start_backends(&m.backend);
        let mut failures: Vec<String> = Vec::new();
        for b in backends {
            match self.try_start_engine(&m, b, port, &api_key).await {
                Ok((child, pid)) => {
                    self.register_provider(&m, port, &api_key)?;
                    {
                        let mut map = self.running.lock().unwrap();
                        map.insert(m.id.clone(), RunningEngine { child: Some(child), pid });
                    }
                    let mut m2 = m.clone();
                    m2.status = "running".to_string();
                    m2.backend = b.as_str().to_string();
                    m2.port = Some(port as i64);
                    m2.updated_at = chrono::Utc::now().timestamp();
                    self.db.save_local_model(&m2).map_err(|e| e.to_string())?;
                    self.broadcast_status(&m2, None);
                    self.spawn_watcher(m2);
                    return Ok(());
                }
                Err(e) => {
                    warn!(id, backend = %b.as_str(), error = %e, "engine start failed, trying next backend");
                    failures.push(format!("{}: {}", b.as_str(), e));
                }
            }
        }
        Err(format!(
            "引擎启动失败（已尝试 {}）: {}",
            failures.len(),
            failures.join("；")
        ))
    }

    async fn try_start_engine(
        &self,
        m: &LocalModel,
        backend: Backend,
        port: u16,
        api_key: &str,
    ) -> anyhow::Result<(tokio::process::Child, u32)> {
        let path = self.model_file_path(&m.id, &m.format);
        if !path.exists() {
            return Err(anyhow::anyhow!("权重文件缺失: {}", path.display()));
        }
        let log_path = self.engines_dir.join(format!("engine-{}.log", m.id));
        let bin = engine::ensure_engine(
            &self.engines_dir,
            backend,
            &self.http_client,
            &AtomicBool::new(false),
        )
        .await?;
        let mut child = engine::spawn_llama_server(
            &bin,
            &path,
            port,
            m.ctx_size,
            m.n_gpu_layers,
            m.thinking == 1,
            api_key,
            &log_path,
        )?;
        let pid = child.id().ok_or_else(|| anyhow::anyhow!("无法获取进程 PID"))?;
        engine::wait_healthy(port, api_key, &self.http_client, START_TIMEOUT, Some(&mut child))
            .await
            .map_err(|e| anyhow::anyhow!("{}（引擎日志: {}）", e, log_path.display()))?;
        Ok((child, pid))
    }

    /// 注册对外 provider + model + key（DB 与内存同步）。
    fn register_provider(&self, m: &LocalModel, port: u16, api_key: &str) -> Result<(), String> {
        let provider_id = format!("local-{}", m.id);
        let now = chrono::Utc::now().timestamp();
        let sort_order = self.db.next_sort_order().unwrap_or(0);

        let provider = Provider {
            id: provider_id.clone(),
            name: format!("本地 · {}", m.model_id),
            kind: ProviderKind::ChatCompletions,
            base_url: format!("http://127.0.0.1:{}", port),
            api_path: "/v1/chat/completions".to_string(),
            config: serde_json::json!({}),
            enabled: true,
            created_at: now,
            updated_at: now,
            sort_order,
        };
        self.db.save_provider(&provider).map_err(|e| e.to_string())?;
        self.providers.insert(provider);

        let model = Model {
            id: format!("local-m-{}", m.id),
            provider_id: provider_id.clone(),
            model_id: m.model_id.clone(),
            display_name: m.model_id.clone(),
            tier: "local".to_string(),
            context_window: m.ctx_size,
            max_output_tokens: 4096,
            capabilities: "[\"text\",\"tools\"]".to_string(),
            enabled: true,
            created_at: now,
            updated_at: now,
        };
        self.db.save_model(&model).map_err(|e| e.to_string())?;

        let key = ApiKey {
            id: format!("local-k-{}", m.id),
            provider_id,
            name: "本地引擎密钥".to_string(),
            key_hash: crate::crypto::encrypt(api_key, &self.master_key)
                .map_err(|e| e.to_string())?,
            key_masked: format!("{}…{}", &api_key[..4], &api_key[api_key.len() - 4..]),
            key_plain: None,
            status: "green".to_string(),
            last_error: None,
            last_error_code: None,
            last_error_time: None,
            last_used_at: None,
            balance: None,
            balance_updated_at: None,
            total_requests: 0,
            total_tokens: 0,
            created_at: now,
            updated_at: now,
        };
        self.db.save_api_key(&key).map_err(|e| e.to_string())?;
        self.keys
            .load_keys_from_db(&key.provider_id, &self.db, &self.master_key)
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// 退出 watcher：崩溃自动重启（5s/15s/45s 退避，最多 3 次）。
    fn spawn_watcher(&self, m: LocalModel) {
        let this = self.clone();
        tokio::spawn(async move {
            loop {
                // 等待子进程退出（不 take，让 stop() 负责清理）
                let status = {
                    let mut map = this.running.lock().unwrap();
                    match map.get_mut(&m.id) {
                        Some(entry) => {
                            if let Some(child) = entry.child.as_mut() {
                                // 使用 try_wait 非阻塞检查
                                match child.try_wait() {
                                    Ok(Some(status)) => Some(status),
                                    Ok(None) => None, // 还在运行
                                    Err(_) => None,
                                }
                            } else {
                                // child 已被 take 或不存在
                                return;
                            }
                        }
                        None => return, // 已从 map 移除
                    }
                };

                if let Some(status) = status {
                    // 子进程已退出
                    info!(id = %m.id, status = ?status, "engine exited");

                    // 从 map 中移除
                    this.running.lock().unwrap().remove(&m.id);

                    let mut m2 = m.clone();
                    m2.status = "error".to_string();
                    m2.updated_at = chrono::Utc::now().timestamp();
                    let _ = this.db.save_local_model(&m2);
                    this.broadcast_status(&m2, Some(format!("引擎退出: {:?}", status)));

                    let mut restarted = false;
                    for backoff in RESTART_BACKOFFS {
                        tokio::time::sleep(backoff).await;
                        if this.running.lock().unwrap().get(&m.id).is_none() {
                            // 已被外部移除（stop），不再重启
                            return;
                        }
                        let port = m2.port.unwrap_or(0) as u16;
                        let api_key = random_api_key();
                        match this
                            .try_start_engine(&m2, Backend::from_str(&m2.backend), port, &api_key)
                            .await
                        {
                            Ok((child, pid)) => {
                                if this.register_provider(&m2, port, &api_key).is_err() {
                                    continue;
                                }
                                this.running.lock().unwrap().insert(
                                    m.id.clone(),
                                    RunningEngine { child: Some(child), pid },
                                );
                                m2.status = "running".to_string();
                                m2.updated_at = chrono::Utc::now().timestamp();
                                let _ = this.db.save_local_model(&m2);
                                this.broadcast_status(&m2, None);
                                restarted = true;
                                break;
                            }
                            Err(e) => {
                                error!(id = %m.id, error = %e, "restart attempt failed");
                            }
                        }
                    }
                    if !restarted {
                        error!(id = %m.id, "engine restart exhausted, giving up");
                        return;
                    }
                } else {
                    // 还在运行，等一会儿再检查
                    tokio::time::sleep(Duration::from_millis(500)).await;
                }
            }
        });
    }

    /// 停止：kill 进程 + 下线 provider（enabled=0，DB 与内存同步）。
    pub async fn stop(&self, id: &str) -> Result<(), String> {
        // 先获取 PID 和 child 句柄
        let entry = {
            let mut map = self.running.lock().unwrap();
            map.remove(id)
        };

        if let Some(mut entry) = entry {
            // 使用 PID 杀死进程（跨平台）
            #[cfg(unix)]
            {
                use std::process::Command;
                let _ = Command::new("kill")
                    .arg("-9")
                    .arg(entry.pid.to_string())
                    .output();
            }
            #[cfg(windows)]
            {
                use std::process::Command;
                let _ = Command::new("taskkill")
                    .arg("/F")
                    .arg("/PID")
                    .arg(entry.pid.to_string())
                    .output();
            }

            // 等待 child 句柄确认进程退出
            if let Some(mut child) = entry.child.take() {
                let _ = child.wait().await;
            }
        }

        let m = self
            .db
            .get_local_model(id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "本地模型不存在".to_string())?;
        let provider_id = format!("local-{}", id);
        self.disable_provider(&provider_id)?;
        self.keys.remove_provider(&provider_id);

        let mut m2 = m;
        m2.status = "downloaded".to_string();
        m2.port = None;
        m2.updated_at = chrono::Utc::now().timestamp();
        self.db.save_local_model(&m2).map_err(|e| e.to_string())?;
        self.broadcast_status(&m2, None);
        Ok(())
    }

    fn disable_provider(&self, provider_id: &str) -> Result<(), String> {
        if let Some(mut p) = self.providers.get(provider_id) {
            p.enabled = false;
            p.updated_at = chrono::Utc::now().timestamp();
            self.db.save_provider(&p).map_err(|e| e.to_string())?;
            self.providers.insert(p);
        }
        Ok(())
    }

    /// 删除：停止引擎 → 删除 provider（级联 models/keys）→ 删 DB 行 → 可选删权重文件。
    pub async fn delete(&self, id: &str, remove_files: bool) -> Result<(), String> {
        self.stop(id).await?;
        let provider_id = format!("local-{}", id);
        self.db
            .delete_provider(&provider_id)
            .map_err(|e| e.to_string())?;
        self.providers.remove(&provider_id);
        self.keys.remove_provider(&provider_id);
        self.db
            .delete_local_model(id)
            .map_err(|e| e.to_string())?;
        if remove_files {
            let dir = self.models_dir.join(id);
            if dir.exists() {
                let _ = std::fs::remove_dir_all(&dir);
            }
        }
        Ok(())
    }

    /// 应用启动 autostart：status=downloaded 且 autostart=1 的模型顺序启动。
    pub async fn auto_start_all(&self) {
        let models = match self.db.list_local_models() {
            Ok(v) => v,
            Err(e) => {
                error!(error = %e, "failed to list local models for autostart");
                return;
            }
        };
        for m in models {
            if m.autostart == 1 && m.status != "downloading" {
                if let Err(e) = self.start(&m.id).await {
                    error!(id = %m.id, error = %e, "autostart failed");
                }
            }
        }
    }

    // ---------- 事件 ----------

    fn broadcast_status(&self, m: &LocalModel, error: Option<String>) {
        let _ = self.events_tx.send(serde_json::json!({
            "type": "local_status",
            "id": m.id,
            "model_id": m.model_id,
            "status": m.status,
            "port": m.port,
            "error": error,
        }));
    }
}

/// 解析后端：auto → 检测首个可用候选（跳过 cpu）。
fn resolve_backend(requested: &str, _format: &str) -> String {
    let b = Backend::from_str(requested);
    if b != Backend::Auto {
        return b.as_str().to_string();
    }
    let detect = backend::detect();
    detect
        .candidates
        .iter()
        .find(|c| c.available && c.backend != "cpu")
        .or_else(|| detect.candidates.iter().find(|c| c.available))
        .map(|c| c.backend.clone())
        .unwrap_or_else(|| "cpu".to_string())
}

/// 启动后端序列：首选 + CPU 兜底（GPU 驱动缺失时降级）。
fn start_backends(backend: &str) -> Vec<Backend> {
    let primary = Backend::from_str(backend);
    if primary == Backend::Cpu || primary == Backend::Auto {
        vec![Backend::Cpu]
    } else {
        vec![primary, Backend::Cpu]
    }
}

/// 32 位十六进制随机密钥（引擎 API key）。
fn random_api_key() -> String {
    use rand::RngCore;
    let mut buf = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut buf);
    buf.iter().map(|b| format!("{:02x}", b)).collect()
}
