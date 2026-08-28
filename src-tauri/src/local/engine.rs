//! 引擎生命周期：llama.cpp 预编译二进制下载/解压 + llama-server 子进程。
//!
//! GGUF 格式使用 llama-server（`ggml-org/llama.cpp` releases，tag 见 `backend::LOCAL_LLAMA_TAG`）。
//! 契约见 docs/specs/spec-local-models.md。

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use tokio::process::Command;
use tracing::{info, warn};

use super::backend::{asset_candidates, cudart_asset, Backend, Platform};

/// 引擎二进制缓存目录：data_dir/engines/{tag}/{platform}-{backend}/
pub fn engine_bin_dir(engines_dir: &Path, backend: Backend) -> PathBuf {
    let platform = Platform::current()
        .map(|p| p.as_str())
        .unwrap_or("unknown");
    engines_dir
        .join(format!("b{}", super::backend::LOCAL_LLAMA_TAG))
        .join(format!("{}-{}", platform, backend.as_str()))
}

/// 确保引擎二进制就绪（下载 + 解压 + 权限），返回 llama-server 可执行文件路径。
pub async fn ensure_engine(
    engines_dir: &Path,
    backend: Backend,
    client: &reqwest::Client,
    cancel: &AtomicBool,
) -> anyhow::Result<PathBuf> {
    let dir = engine_bin_dir(engines_dir, backend);
    if let Some(b) = find_llama_server(&dir).await {
        return Ok(b);
    }

    let tag = super::backend::LOCAL_LLAMA_TAG;
    let candidates = asset_candidates(backend);
    let mut last_err = "no asset candidates".to_string();

    for asset in &candidates {
        let direct = format!(
            "https://github.com/ggml-org/llama.cpp/releases/download/b{}/{}",
            tag, asset
        );
        info!(backend = backend.as_str(), asset, "Downloading llama.cpp engine");
        let zip_path = std::env::temp_dir().join(asset);
        let urls = download_url_chain(&direct);
        let mut downloaded = false;
        for (j, url) in urls.iter().enumerate() {
            info!(url = %url, "trying engine download source {}/{}", j + 1, urls.len());
            match download_file(client, url, &zip_path, cancel).await {
                Ok(_) => {
                    downloaded = true;
                    break;
                }
                Err(e) => {
                    warn!(url = %url, error = %e, "engine download source failed");
                    last_err = format!("{}: {}", url, e);
                    let _ = std::fs::remove_file(&zip_path);
                }
            }
        }
        if downloaded {
            if let Err(e) = extract_archive(&zip_path, &dir) {
                warn!(error = %e, "engine extraction failed");
                last_err = format!("extract failed: {}", e);
                let _ = std::fs::remove_file(&zip_path);
                continue;
            }
            let _ = std::fs::remove_file(&zip_path);
            set_exec_permission(&dir);
            if let Some(b) = find_llama_server(&dir).await {
                return Ok(b);
            }
            last_err = "binary not found in archive".to_string();
        }
    }

    // CUDA 专属：解压 cudart DLL 到同一目录（Win 新 release 拆分了运行时）
    if backend == Backend::Cuda {
        if let Some(cudart) = cudart_asset() {
            let direct = format!(
                "https://github.com/ggml-org/llama.cpp/releases/download/b{}/{}",
                tag, cudart
            );
            let zip_path = std::env::temp_dir().join(&cudart);
            for url in download_url_chain(&direct) {
                if download_file(client, &url, &zip_path, cancel).await.is_ok() {
                    if let Err(e) = extract_archive(&zip_path, &dir) {
                        warn!(error = %e, "cudart extraction failed");
                    }
                    break;
                }
            }
            let _ = std::fs::remove_file(&zip_path);
        }
    }

    if let Some(b) = find_llama_server(&dir).await {
        return Ok(b);
    }
    Err(anyhow::anyhow!(
        "engine binary unavailable for backend '{}': {}",
        backend.as_str(),
        last_err
    ))
}

/// 引擎下载源链：直连 + 国内加速镜像（顺序尝试，前序失败才用镜像）。
fn download_url_chain(direct: &str) -> Vec<String> {
    vec![
        direct.to_string(),
        format!("https://gh-proxy.com/{}", direct),
        format!("https://ghfast.top/{}", direct),
    ]
}

async fn download_file(
    client: &reqwest::Client,
    url: &str,
    dest: &Path,
    cancel: &AtomicBool,
) -> anyhow::Result<()> {
    let mut last_err: Option<String> = None;
    for attempt in 0..3 {
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(anyhow::anyhow!("cancelled"));
        }
        if attempt > 0 {
            tokio::time::sleep(Duration::from_secs(2 * attempt as u64)).await;
            info!(attempt, url, "retrying engine download");
        }
        match download_file_once(client, url, dest, cancel).await {
            Ok(()) => return Ok(()),
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("cancelled") {
                    return Err(e);
                }
                warn!(attempt, error = %e, "engine download attempt failed");
                last_err = Some(msg);
                let _ = std::fs::remove_file(dest);
            }
        }
    }
    Err(anyhow::anyhow!(
        "{}（已重试 3 次，可检查系统代理后重试）",
        last_err.unwrap_or_else(|| "unknown error".to_string())
    ))
}

async fn download_file_once(
    client: &reqwest::Client,
    url: &str,
    dest: &Path,
    cancel: &AtomicBool,
) -> anyhow::Result<()> {
    let resp = client.get(url).send().await?;
    if !resp.status().is_success() {
        return Err(anyhow::anyhow!("HTTP {}", resp.status()));
    }
    let total = resp.content_length().unwrap_or(0);
    let mut out = std::fs::File::create(dest)?;
    let mut stream = resp.bytes_stream();
    let mut downloaded: u64 = 0;
    use futures::StreamExt;
    while let Some(chunk) = stream.next().await {
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(anyhow::anyhow!("cancelled"));
        }
        let chunk = chunk?;
        std::io::Write::write_all(&mut out, &chunk)?;
        downloaded += chunk.len() as u64;
    }
    if total > 0 && downloaded != total {
        return Err(anyhow::anyhow!(
            "size mismatch: expected {} bytes, got {}",
            total,
            downloaded
        ));
    }
    Ok(())
}

/// 解压 .zip / .tar.gz 到目标目录（按扩展名分派；路径穿越防护）。
fn extract_archive(archive_path: &Path, out_dir: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(out_dir)?;
    let name = archive_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");
    if name.ends_with(".zip") {
        extract_zip(archive_path, out_dir)
    } else if name.ends_with(".tar.gz") || name.ends_with(".tgz") {
        extract_tar_gz(archive_path, out_dir)
    } else {
        Err(anyhow::anyhow!("unknown archive format: {}", name))
    }
}

fn extract_zip(zip_path: &Path, out_dir: &Path) -> anyhow::Result<()> {
    let file = std::fs::File::open(zip_path)?;
    let mut archive = zip::ZipArchive::new(file)?;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i)?;
        let name = entry.name().to_string();
        let clean = name.replace('\\', "/");
        if clean.starts_with('/') || clean.contains("..") {
            continue;
        }
        let out_path = out_dir.join(&clean);
        if entry.is_dir() {
            std::fs::create_dir_all(&out_path)?;
            continue;
        }
        if let Some(parent) = out_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut out = std::fs::File::create(&out_path)?;
        std::io::copy(&mut entry, &mut out)?;
    }
    Ok(())
}

fn extract_tar_gz(path: &Path, out_dir: &Path) -> anyhow::Result<()> {
    let file = std::fs::File::open(path)?;
    let decoder = flate2::read::GzDecoder::new(file);
    let mut archive = tar::Archive::new(decoder);
    archive.unpack(out_dir)?;
    Ok(())
}

/// 递归查找 llama-server 可执行文件。
async fn find_llama_server(dir: &Path) -> Option<PathBuf> {
    if !dir.exists() {
        return None;
    }
    let exe = if cfg!(target_os = "windows") {
        "llama-server.exe"
    } else {
        "llama-server"
    };
    let mut stack: Vec<PathBuf> = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let entries = std::fs::read_dir(&d).ok()?;
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.file_name().and_then(|n| n.to_str()) == Some(exe) {
                return Some(p);
            }
        }
    }
    None
}

#[cfg(unix)]
fn set_exec_permission(dir: &Path) {
    if let Some(bin) = find_llama_server_sync(dir) {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755));
    }
}

#[cfg(not(unix))]
fn set_exec_permission(_dir: &Path) {}

#[cfg(unix)]
fn find_llama_server_sync(dir: &Path) -> Option<PathBuf> {
    let exe = "llama-server";
    let mut stack: Vec<PathBuf> = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let entries = std::fs::read_dir(&d).ok()?;
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.file_name().and_then(|n| n.to_str()) == Some(exe) {
                return Some(p);
            }
        }
    }
    None
}

/// 启动 GGUF 引擎（llama-server），stdout/stderr 写入 log_path（覆盖式）。
pub fn spawn_llama_server(
    bin: &Path,
    model_path: &Path,
    port: u16,
    ctx_size: i64,
    n_gpu_layers: i64,
    api_key: &str,
    log_path: &Path,
) -> std::io::Result<tokio::process::Child> {
    info!(bin = %bin.display(), port, "spawning llama-server");
    let log_file = std::fs::File::create(log_path)?;
    let log_err = log_file.try_clone()?;
    let mut cmd = Command::new(bin);
    cmd.arg("--model")
        .arg(model_path)
        .arg("--host")
        .arg("127.0.0.1")
        .arg("--port")
        .arg(port.to_string())
        .arg("--ctx-size")
        .arg(ctx_size.to_string())
        .arg("--n-gpu-layers")
        .arg(n_gpu_layers.to_string())
        .arg("--api-key")
        .arg(api_key)
        .arg("--parallel")
        .arg("1")
        .stdout(std::process::Stdio::from(log_file))
        .stderr(std::process::Stdio::from(log_err));
    cmd.spawn()
}

/// 轮询引擎健康（GET /v1/models，Bearer 认证），超时返回 Err。
pub async fn wait_healthy(
    port: u16,
    api_key: &str,
    client: &reqwest::Client,
    timeout: Duration,
    mut child: Option<&mut tokio::process::Child>,
) -> anyhow::Result<()> {
    let url = format!("http://127.0.0.1:{}/v1/models", port);
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if std::time::Instant::now() > deadline {
            return Err(anyhow::anyhow!(
                "engine health check timed out after {:?}",
                timeout
            ));
        }
        if let Some(c) = child.as_mut() {
            if let Ok(Some(status)) = c.try_wait() {
                return Err(anyhow::anyhow!(
                    "engine process exited during startup: {}（详见引擎日志）",
                    status
                ));
            }
        }
        let resp = client
            .get(&url)
            .header("Authorization", format!("Bearer {}", api_key))
            .timeout(Duration::from_secs(3))
            .send()
            .await;
        match resp {
            Ok(r) if r.status().is_success() => return Ok(()),
            _ => tokio::time::sleep(Duration::from_millis(800)).await,
        }
    }
}

/// 获取一个空闲端口（bind 0 探测后释放；竞争概率低）。
pub fn free_port() -> anyhow::Result<u16> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    Ok(listener.local_addr()?.port())
}
