# vendor/block

`block` 0.1.6 的 vendor 补丁副本。

## 为什么需要 vendor

`block` 是 `souvlaki` → `cocoa` 在 macOS 上的传递依赖。上游自 2016 年后未再更新，
其 `src/lib.rs` 中定义了一个空枚举：

```rust
enum Class { }
```

它随后被用作 FFI 静态量的类型：

```rust
extern {
    static _NSConcreteStackBlock: Class;
    ...
}
```

这会触发未来不兼容 lint `uninhabited-static`（rust-lang/rust#74840），
在较新的 rustc 上产生编译警告甚至错误。

## 补丁内容

`src/lib.rs` 中将空枚举替换为带私有字段的 opaque struct，
使其不再是 uninhabited type，同时保留「不可在 Rust 侧构造」的语义：

```rust
// patched
struct Class { _private: [u8; 0] }
```

该类型仅作为 Objective-C runtime 的哨兵地址使用，
Rust 侧从不构造或匹配其值，因此改动对运行时行为无影响。

## 升级

若上游发布新版修复此问题，可移除本目录并将 `Cargo.toml` 中
`[patch.crates-io]` 的 `block = { path = "vendor/block" }` 注释掉。
