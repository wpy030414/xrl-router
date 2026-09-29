// 载体文件：使 cargo:rustc-link-arg-tests 合法化的最小集成测试 target（清单经 build.rs 嵌入测试产物）。不放实际测试逻辑。
#[test]
fn manifest_carrier() {
    assert!(true);
}
