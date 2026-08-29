//! 系统资源监控（CPU/内存/显存占用）
//! 为前端提供实时系统资源使用情况

use serde::Serialize;
use sysinfo::System;
use std::sync::Mutex;

/// 系统资源使用情况
#[derive(Debug, Clone, Serialize)]
pub struct SystemResources {
    /// CPU 使用率（0-100）
    pub cpu_usage: f32,
    /// 已用内存（字节）
    pub used_memory: u64,
    /// 总内存（字节）
    pub total_memory: u64,
}

/// 系统监控器（线程安全）
pub struct SystemMonitor {
    system: Mutex<System>,
}

impl SystemMonitor {
    /// 创建新的系统监控器
    pub fn new() -> Self {
        let mut system = System::new_all();
        // 等待第一次 CPU 采样（需要两次采样才能计算使用率）
        std::thread::sleep(std::time::Duration::from_millis(200));
        system.refresh_all();

        Self {
            system: Mutex::new(system),
        }
    }

    /// 获取当前系统资源使用情况
    pub fn get_resources(&self) -> SystemResources {
        let mut system = self.system.lock().unwrap();

        // 刷新 CPU 和内存信息
        system.refresh_cpu_all();
        system.refresh_memory();

        // 计算平均 CPU 使用率
        let cpu_usage = system.global_cpu_usage();

        // 内存信息
        let used_memory = system.used_memory();
        let total_memory = system.total_memory();

        SystemResources {
            cpu_usage,
            used_memory,
            total_memory,
        }
    }
}

impl Default for SystemMonitor {
    fn default() -> Self {
        Self::new()
    }
}
