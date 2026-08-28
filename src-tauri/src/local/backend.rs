//! 计算后端检测：平台/GPU 探测 → 候选后端优先级列表。
//!
//! 契约见 docs/specs/spec-local-models.md。
//! 检测结果缓存（进程生命周期内不变）；`auto` 模式下按优先级降级尝试。

use std::sync::OnceLock;

/// llama.cpp 官方 release tag（ggml-org/llama.cpp）。
/// 资产命名随版本变化，升级时需同步核对 `asset_candidates` 与 `cudart_asset`。
/// llama.cpp release tag 的纯数字部分（URL 拼装统一加 `b` 前缀，避免双 b）。
pub const LOCAL_LLAMA_TAG: &str = "10448";

/// 后端标识（与 DB local_models.backend 一致）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    Auto,
    Cpu,
    Cuda,
    Vulkan,
    Rocm,
    Metal,
}

impl Backend {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Cpu => "cpu",
            Self::Cuda => "cuda",
            Self::Vulkan => "vulkan",
            Self::Rocm => "rocm",
            Self::Metal => "metal",
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s {
            "cpu" => Self::Cpu,
            "cuda" => Self::Cuda,
            "vulkan" => Self::Vulkan,
            "rocm" => Self::Rocm,
            "metal" => Self::Metal,
            _ => Self::Auto,
        }
    }
}

/// 平台标识（跨平台构建时未命中平台会产生未构造警告，属预期）。
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    MacosArm64,
    MacosX64,
    WindowsX64,
    LinuxX64,
}

impl Platform {
    pub fn current() -> Option<Self> {
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        return Some(Self::MacosArm64);
        #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
        return Some(Self::MacosX64);
        #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
        return Some(Self::WindowsX64);
        #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
        return Some(Self::LinuxX64);
        #[allow(unreachable_code)]
        None
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::MacosArm64 => "macos-arm64",
            Self::MacosX64 => "macos-x64",
            Self::WindowsX64 => "windows-x64",
            Self::LinuxX64 => "linux-x64",
        }
    }
}

/// 后端检测结果（/api/local/backends 返回）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct BackendDetect {
    pub platform: String,
    pub arch: String,
    /// auto 模式下的候选顺序（含检测依据）。
    pub candidates: Vec<BackendCandidate>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct BackendCandidate {
    pub backend: String,
    pub available: bool,
    pub reason: String,
}

/// 进程级缓存（GPU 检测开销 ~100ms，一次即可）。
fn detect_cache() -> &'static OnceLock<BackendDetect> {
    static CACHE: OnceLock<BackendDetect> = OnceLock::new();
    &CACHE
}

fn command_in_path(cmd: &str) -> bool {
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("where")
            .arg(cmd)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }
    #[cfg(not(target_os = "windows"))]
    {
        std::process::Command::new(cmd)
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }
}

/// 检测当前平台的后端候选（auto 模式优先级）。
pub fn detect() -> BackendDetect {
    if let Some(d) = detect_cache().get() {
        return d.clone();
    }

    let platform = Platform::current()
        .map(|p| p.as_str().to_string())
        .unwrap_or_else(|| "unsupported".to_string());
    let arch = std::env::consts::ARCH.to_string();

    let mut candidates: Vec<BackendCandidate> = Vec::new();
    match Platform::current() {
        Some(Platform::MacosArm64) | Some(Platform::MacosX64) => {
            candidates.push(BackendCandidate {
                backend: Backend::Metal.as_str().to_string(),
                available: true,
                reason: "macOS 默认 Metal".to_string(),
            });
            candidates.push(BackendCandidate {
                backend: Backend::Cpu.as_str().to_string(),
                available: true,
                reason: "CPU 兜底".to_string(),
            });
        }
        Some(Platform::WindowsX64) => {
            let nvidia = command_in_path("nvidia-smi");
            candidates.push(BackendCandidate {
                backend: Backend::Cuda.as_str().to_string(),
                available: nvidia,
                reason: if nvidia {
                    "nvidia-smi 命中".to_string()
                } else {
                    "未检测到 NVIDIA GPU".to_string()
                },
            });
            let amd = windows_has_amd_gpu();
            candidates.push(BackendCandidate {
                backend: Backend::Rocm.as_str().to_string(),
                available: amd,
                reason: if amd {
                    "检测到 AMD/Radeon GPU".to_string()
                } else {
                    "未检测到 AMD GPU".to_string()
                },
            });
            candidates.push(BackendCandidate {
                backend: Backend::Vulkan.as_str().to_string(),
                available: true,
                reason: "Vulkan 跨厂商兜底".to_string(),
            });
            candidates.push(BackendCandidate {
                backend: Backend::Cpu.as_str().to_string(),
                available: true,
                reason: "CPU 兜底".to_string(),
            });
        }
        Some(Platform::LinuxX64) => {
            let nvidia = command_in_path("nvidia-smi");
            candidates.push(BackendCandidate {
                backend: Backend::Cuda.as_str().to_string(),
                available: nvidia,
                reason: if nvidia {
                    "nvidia-smi 命中".to_string()
                } else {
                    "未检测到 NVIDIA GPU".to_string()
                },
            });
            let vulkan = command_in_path("vulkaninfo");
            candidates.push(BackendCandidate {
                backend: Backend::Vulkan.as_str().to_string(),
                available: vulkan,
                reason: if vulkan {
                    "vulkaninfo 命中".to_string()
                } else {
                    "未安装 vulkaninfo".to_string()
                },
            });
            candidates.push(BackendCandidate {
                backend: Backend::Cpu.as_str().to_string(),
                available: true,
                reason: "CPU 兜底".to_string(),
            });
        }
        None => {}
    }

    let detect = BackendDetect {
        platform,
        arch,
        candidates,
    };
    let _ = detect_cache().set(detect.clone());
    detect
}

/// Windows AMD GPU 探测（wmic 显卡名含 AMD/Radeon）。
#[cfg(target_os = "windows")]
fn windows_has_amd_gpu() -> bool {
    let out = std::process::Command::new("wmic")
        .args(["path", "win32_VideoController", "get", "name"])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_lowercase())
        .unwrap_or_default();
    out.contains("amd") || out.contains("radeon")
}

#[cfg(not(target_os = "windows"))]
fn windows_has_amd_gpu() -> bool {
    false
}

/// llama.cpp release 资产候选名（按顺序尝试，首个 200 生效）。
/// tag 升级时资产命名可能变化，这里用多候选兜底。
pub fn asset_candidates(backend: Backend) -> Vec<String> {
    let tag = LOCAL_LLAMA_TAG;
    match backend {
        Backend::Metal => {
            #[cfg(target_arch = "aarch64")]
            let arch = "arm64";
            #[cfg(target_arch = "x86_64")]
            let arch = "x64";
            vec![format!("llama-b{tag}-bin-macos-{arch}.tar.gz")]
        }
        Backend::Cuda => match Platform::current() {
            Some(Platform::WindowsX64) => vec![
                format!("llama-b{tag}-bin-win-cuda-12.4-x64.zip"),
                format!("llama-b{tag}-bin-win-cuda-x64.zip"),
            ],
            _ => vec![
                format!("llama-b{tag}-bin-ubuntu-cuda-x64.tar.gz"),
                format!("llama-b{tag}-bin-ubuntu-cuda-12-x64.tar.gz"),
            ],
        },
        Backend::Vulkan => match Platform::current() {
            Some(Platform::WindowsX64) => vec![format!("llama-b{tag}-bin-win-vulkan-x64.zip")],
            _ => vec![format!("llama-b{tag}-bin-ubuntu-vulkan-x64.tar.gz")],
        },
        Backend::Rocm => match Platform::current() {
            Some(Platform::WindowsX64) => vec![
                format!("llama-b{tag}-bin-win-rocm-7.14-x64.zip"),
                format!("llama-b{tag}-bin-win-hip-x64.zip"),
            ],
            _ => vec![],
        },
        Backend::Cpu => match Platform::current() {
            Some(Platform::WindowsX64) => vec![
                format!("llama-b{tag}-bin-win-cpu-x64.zip"),
                format!("llama-b{tag}-bin-win-cpu-avx2-x64.zip"),
            ],
            Some(Platform::MacosArm64) => {
                vec![format!("llama-b{tag}-bin-macos-arm64.tar.gz")]
            }
            Some(Platform::MacosX64) => vec![format!("llama-b{tag}-bin-macos-x64.tar.gz")],
            _ => vec![format!("llama-b{tag}-bin-ubuntu-x64.tar.gz")],
        },
        _ => vec![],
    }
}

/// Windows CUDA 运行时 DLL 资产（新 release 把 cudart 拆成独立 zip）。
pub fn cudart_asset() -> Option<String> {
    if Platform::current() == Some(Platform::WindowsX64) {
        Some("cudart-llama-bin-win-cuda-12.4-x64.zip".to_string())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_returns_candidates() {
        let d = detect();
        assert!(!d.candidates.is_empty(), "应始终有候选（CPU 兜底）");
        assert!(d.candidates.iter().any(|c| c.backend == "cpu" && c.available));
    }

    #[test]
    fn test_asset_candidates_non_empty_for_common() {
        for b in [Backend::Cpu, Backend::Vulkan, Backend::Metal, Backend::Cuda] {
            let assets = asset_candidates(b);
            assert!(!assets.is_empty(), "{:?} 应有资产候选", b);
            for a in assets {
                assert!(a.contains("llama-b"), "资产名应含前缀: {}", a);
            }
        }
    }

    /// 回归：LOCAL_LLAMA_TAG 是纯数字，拼装出的 URL/资产名只能有一个 b 前缀。
    #[test]
    fn test_tag_no_double_b() {
        assert!(
            LOCAL_LLAMA_TAG.chars().all(|c| c.is_ascii_digit()),
            "LOCAL_LLAMA_TAG 应为纯数字（URL 拼装自带 b 前缀），实际: {}",
            LOCAL_LLAMA_TAG
        );
        let expect = format!("llama-b{}-bin-", LOCAL_LLAMA_TAG);
        for a in asset_candidates(Backend::Cpu) {
            assert!(
                a.starts_with(&expect),
                "资产名应形如 llama-b{{tag}}-bin-...: {}",
                a
            );
            assert!(!a.contains("bb"), "资产名不应出现双 b: {}", a);
        }
    }
}
