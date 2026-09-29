fn main() {
    // Windows/MSVC：给 cargo test 产物嵌入应用清单（tests.manifest）。
    //
    // tauri-build 只为主程序嵌 RT_MANIFEST；测试 exe 缺清单时加载器把
    // comctl32 绑到旧 v5 → 缺 v6 入口点 → 测试进程加载期失败
    // （0xC0000139 STATUS_ENTRYPOINT_NOT_FOUND），报错不指明缺失符号。
    // cargo:rustc-link-arg-tests 只作用于 test/bench 产物，主程序不受影响。
    let target_env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os == "windows" && target_env == "msvc" {
        let manifest = std::path::Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap())
            .join("tests.manifest");
        println!("cargo:rustc-link-arg-tests=/MANIFEST:EMBED");
        println!("cargo:rustc-link-arg-tests=/MANIFESTINPUT:{}", manifest.display());
        println!("cargo:rerun-if-changed={}", manifest.display());
    }

    tauri_build::build()
}
