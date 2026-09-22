//! 插件伴生启动（进程托管）：Router 自动拉起 TS + Hono 反向代理网关。
//!
//! 契约（docs/specs/module-plugin-system.md）：
//! - 每个插件实现是一个 TypeScript + Hono 网关项目，package.json 提供
//!   `serve`（启动网关 + WS 连 Router 注册，幂等）与 `login`（弹系统浏览器
//!   完成登录）脚本。
//! - `plugins.autostart = 1` 且 `work_dir` 非空时，Router 启动后自动
//!   `pnpm run serve` 拉起；已启动（TCP 探测连通）则不重复拉起，
//!   serve 脚本自身的幂等性作最终兜底。
//! - 退出时只回收 **Router 自己 spawn** 的进程（用户手动启动的不动）。
//! - 前置条件：系统内 node 与 pnpm 可用，否则托管功能不可用。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};

use serde::Serialize;
use tracing::{info, warn, error};

use crate::db::Database;

/// node / pnpm 探测结果（版本串，None = 不可用）。
#[derive(Debug, Clone, Serialize)]
pub struct RuntimeStatus {
    pub node: Option<String>,
    pub pnpm: Option<String>,
}

impl RuntimeStatus {
    pub fn available(&self) -> bool {
        self.node.is_some() && self.pnpm.is_some()
    }
}

/// Router 托管的子进程句柄。child 取走后（kill 时）仅剩 pid 供记录。
struct HostedChild {
    child: Option<tokio::process::Child>,
    #[allow(dead_code)]
    pid: Option<u32>,
}

/// 管理插件网关的进程生命周期。
#[derive(Clone)]
pub struct PluginProcessManager {
    db: Database,
    /// 日志目录：{data_dir}/logs/plugins/
    logs_dir: PathBuf,
    /// 只记录 Router 自己 spawn 的 serve 进程（plugin_id → child）
    running: Arc<Mutex<HashMap<String, HostedChild>>>,
    /// node/pnpm 探测缓存（进程内一次）
    runtime: Arc<RwLock<Option<RuntimeStatus>>>,
}

impl PluginProcessManager {
    pub fn new(db: Database, data_dir: PathBuf) -> Self {
        Self {
            db,
            logs_dir: data_dir.join("logs").join("plugins"),
            running: Arc::new(Mutex::new(HashMap::new())),
            runtime: Arc::new(RwLock::new(None)),
        }
    }

    /// 探测系统内 node / pnpm 可用性（带进程内缓存；force 强制重探）。
    pub async fn check_runtime(&self, force: bool) -> RuntimeStatus {
        if !force {
            if let Ok(guard) = self.runtime.read() {
                if let Some(cached) = guard.as_ref() {
                    return cached.clone();
                }
            }
        }
        let node = probe_version("node").await;
        let pnpm = probe_version(pnpm_bin()).await;
        let status = RuntimeStatus { node, pnpm };
        if let Ok(mut guard) = self.runtime.write() {
            *guard = Some(status.clone());
        }
        status
    }

    /// `pnpm run serve` 拉起插件网关（幂等：已在托管列表则跳过）。
    /// stdout/stderr 落 `{logs_dir}/{plugin_id}.log`（覆盖式）。
    pub fn spawn_serve(&self, plugin_id: &str, work_dir: &str) -> anyhow::Result<()> {
        {
            let running = self
                .running
                .lock()
                .map_err(|e| anyhow::anyhow!("running map poisoned: {}", e))?;
            if running.contains_key(plugin_id) {
                info!(plugin = %plugin_id, "plugin gateway already hosted, skip spawn");
                return Ok(());
            }
        }
        let child = self.spawn_pnpm(plugin_id, work_dir, "serve")?;
        let pid = child.id();
        if let Ok(mut running) = self.running.lock() {
            running.insert(plugin_id.to_string(), HostedChild { child: Some(child), pid });
        }
        info!(plugin = %plugin_id, work_dir = %work_dir, pid, "plugin serve process hosted");
        Ok(())
    }

    /// `pnpm run login` 启动登录流程（fire-and-forget：login 是短命一次性进程，
    /// 不入托管列表，后台 wait 收尸避免僵尸）。login 脚本契约自行弹系统浏览器。
    pub fn spawn_login(&self, plugin_id: &str, work_dir: &str) -> anyhow::Result<()> {
        let mut child = self.spawn_pnpm(plugin_id, work_dir, "login")?;
        tokio::spawn(async move {
            let _ = child.wait().await;
        });
        info!(plugin = %plugin_id, work_dir = %work_dir, "plugin login process launched");
        Ok(())
    }

    /// 应用启动伴生启动：autostart=1 且 work_dir 非空的插件逐个拉起。
    /// 「已启动不重复启动」：① 已在托管列表 → 跳过；② provider 的 base_url
    /// TCP 探测连通（用户手动启动 / 上次残留实例）→ 跳过；③ serve 幂等契约兜底。
    pub async fn auto_start_all(&self) {
        let rows: Vec<(String, String, Option<String>)> = {
            let conn = self.db.conn();
            let mut stmt = match conn.prepare(
                "SELECT p.id, p.work_dir, pr.base_url FROM plugins p
                 LEFT JOIN providers pr ON p.provider_id = pr.id
                 WHERE p.autostart = 1 AND p.work_dir IS NOT NULL AND p.work_dir != ''",
            ) {
                Ok(s) => s,
                Err(e) => {
                    error!(error = %e, "failed to list plugins for autostart");
                    return;
                }
            };
            let rows = stmt
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, Option<String>>(2)?))
                })
                .map(|rows| rows.filter_map(|r| r.ok()).collect::<Vec<_>>());
            match rows {
                Ok(v) => v,
                Err(e) => {
                    error!(error = %e, "failed to query plugins for autostart");
                    return;
                }
            }
        };

        if rows.is_empty() {
            return;
        }

        let rt = self.check_runtime(false).await;
        if !rt.available() {
            warn!(
                node = ?rt.node,
                pnpm = ?rt.pnpm,
                "node/pnpm unavailable, plugin companion autostart skipped ({} plugin(s) affected)",
                rows.len()
            );
            return;
        }

        for (plugin_id, work_dir, base_url) in rows {
            {
                let Ok(running) = self.running.lock() else { continue };
                if running.contains_key(&plugin_id) {
                    continue;
                }
            }
            // provider 已删除（被忽略的插件）→ 无 base_url，跳过
            let Some(base_url) = base_url else {
                continue;
            };
            if probe_base_url(&base_url).await {
                info!(plugin = %plugin_id, base_url = %base_url, "plugin gateway already running, skip autostart");
                continue;
            }
            if let Err(e) = self.spawn_serve(&plugin_id, &work_dir) {
                error!(plugin = %plugin_id, error = %e, "plugin autostart failed");
            }
        }
    }

    /// 回收指定插件的托管进程（删除插件时调用）。
    pub fn kill(&self, plugin_id: &str) {
        let entry = {
            let Ok(mut running) = self.running.lock() else { return };
            running.remove(plugin_id)
        };
        if let Some(mut hosted) = entry {
            if let Some(child) = hosted.child.take() {
                kill_child(child);
                info!(plugin = %plugin_id, "plugin hosted process killed");
            }
        }
    }

    /// 退出时回收全部托管进程（RunEvent::Exit / 删除插件时调用）。
    /// 只杀 Router 自己 spawn 的——用户手动启动的网关不受影响。
    pub fn kill_all(&self) {
        let entries: Vec<(String, HostedChild)> = {
            let Ok(mut running) = self.running.lock() else { return };
            running.drain().collect()
        };
        for (plugin_id, mut hosted) in entries {
            if let Some(child) = hosted.child.take() {
                kill_child(child);
                info!(plugin = %plugin_id, "plugin hosted process killed (exit cleanup)");
            }
        }
    }

    /// 统一的 pnpm 脚本启动：Windows 用 pnpm.cmd（CreateProcess 只补 .exe，
    /// 找不到 npm 安装的 .cmd）+ CREATE_NO_WINDOW；unix 设独立进程组供整组击杀。
    fn spawn_pnpm(
        &self,
        plugin_id: &str,
        work_dir: &str,
        script: &str,
    ) -> anyhow::Result<tokio::process::Child> {
        std::fs::create_dir_all(&self.logs_dir)?;
        let log_path = if script == "serve" {
            self.logs_dir.join(format!("{}.log", plugin_id))
        } else {
            self.logs_dir.join(format!("{}-{}.log", plugin_id, script))
        };
        let log_file = std::fs::File::create(&log_path)?;
        let log_err = log_file.try_clone()?;

        let mut cmd = tokio::process::Command::new(pnpm_bin());
        cmd.args(["run", script])
            .current_dir(work_dir)
            .stdout(std::process::Stdio::from(log_file))
            .stderr(std::process::Stdio::from(log_err));

        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            // CREATE_NO_WINDOW：release 构建无控制台，避免每次拉起闪一个 cmd 窗
            cmd.as_std_mut().creation_flags(0x0800_0000);
        }
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            // 独立进程组：退出时按 pgid 整组击杀（pnpm → node 祖孙链）
            cmd.as_std_mut().process_group(0);
        }

        cmd.spawn().map_err(|e| {
            anyhow::anyhow!("failed to spawn pnpm run {} in {} ({}): {}", script, work_dir, log_path.display(), e)
        })
    }
}

/// Windows 下 pnpm 是 .cmd 脚本，必须显式指名才能被 CreateProcess 找到。
fn pnpm_bin() -> &'static str {
    if cfg!(windows) { "pnpm.cmd" } else { "pnpm" }
}

/// 跑 `<bin> -v` 拿版本串（5s 超时；失败/超时返回 None）。
async fn probe_version(bin: &str) -> Option<String> {
    let mut cmd = tokio::process::Command::new(bin);
    cmd.arg("-v");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.as_std_mut().creation_flags(0x0800_0000);
    }
    match tokio::time::timeout(std::time::Duration::from_secs(5), cmd.output()).await {
        Ok(Ok(out)) if out.status.success() => {
            Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
        }
        _ => None,
    }
}

/// 对 base_url 做 1s TCP 探测：可连通 = 网关已在跑。
async fn probe_base_url(base_url: &str) -> bool {
    let Ok(url) = url::Url::parse(base_url) else {
        return false;
    };
    let Some(host) = url.host_str() else {
        return false;
    };
    let port = match url.port_or_known_default() {
        Some(p) => p,
        None => return false,
    };
    let addr = format!("{}:{}", host, port);
    matches!(
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            tokio::net::TcpStream::connect(&addr),
        )
        .await,
        Ok(Ok(_))
    )
}

/// 击杀托管子进程：Windows taskkill /T /F（杀祖孙进程树——pnpm.cmd → cmd → node）；
/// unix kill -9 -- -pgid（负 pid 杀整个进程组，fallback 单 pid）。
/// 最后 start_kill() 兜底（句柄仍有效时）。
fn kill_child(child: tokio::process::Child) {
    let Some(pid) = child.id() else { return };
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let _ = std::process::Command::new("taskkill")
            .args(["/T", "/F", "/PID", &pid.to_string()])
            .creation_flags(0x0800_0000)
            .status();
    }
    #[cfg(unix)]
    {
        // 负 pid = 按进程组击杀（spawn 时 process_group(0) 设立）
        let _ = std::process::Command::new("kill")
            .args(["-9", &format!("-{}", pid)])
            .status()
            .or_else(|_| std::process::Command::new("kill").args(["-9", &pid.to_string()]).status());
    }
    // taskkill / kill 失败时用句柄直接杀（无 /T，可能残留孙进程，最后手段）
    let mut child = child;
    let _ = child.start_kill();
}
