# Spec: 认证系统

## 目标

实现双层认证：Service Key（客户端访问）+ Provider API Key（上游调用）。

## Service Key

### 用途

客户端访问 `/v1/messages`、`/v1/chat/completions`、`/v1/models` 的凭证。

### 存储格式

```sql
CREATE TABLE service_keys (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL DEFAULT '',
    key_hash TEXT NOT NULL,        -- Argon2 哈希
    key_masked TEXT NOT NULL,      -- 脱敏显示
    allowed_models TEXT NOT NULL DEFAULT '[]', -- JSON 数组
    quota_5h INTEGER NOT NULL DEFAULT 0,  -- 5h 滚动窗口 token 上限（0 = 不设限）
    quota_7d INTEGER NOT NULL DEFAULT 0,  -- 7d 滚动窗口 token 上限（0 = 不设限）
    total_requests INTEGER DEFAULT 0,
    total_tokens INTEGER DEFAULT 0,
    last_used_at INTEGER,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);
```

### 哈希算法

**Argon2id**（内存硬算法，抗 GPU/ASIC 攻击）

```rust
pub fn hash_service_key(raw_key: &str) -> Result<String> {
    let salt = SaltString::generate(&mut OsRng);
    let argon2 = Argon2::default();
    let hash = argon2.hash_password(raw_key.as_bytes(), &salt)?;
    Ok(hash.to_string())
}
```

**参数**:
- 算法: Argon2id（使用 `Argon2::default()`，参数由 crate 默认值决定）
- 版本: 19 (0x13)
- 内存: 19456 KiB (~19 MiB)
- 迭代: 2
- 并行: 1

**注意**: 参数未显式固定，依赖 crate 默认值——升级 crate 可能改变参数。

### 验证逻辑

```rust
pub fn verify_service_key(raw_key: &str, hash: &str) -> Result<bool> {
    let parsed = PasswordHash::new(hash)?;
    Ok(Argon2::default()
        .verify_password(raw_key.as_bytes(), &parsed)
        .is_ok())
}
```

**注意**: 需要逐条遍历所有 Service Key（无法索引查找）。

## Provider API Key

### 用途

调用上游 LLM API（Anthropic、OpenAI）的凭证。

### 存储格式

```sql
CREATE TABLE api_keys (
    id TEXT PRIMARY KEY,
    provider_id TEXT NOT NULL,
    name TEXT NOT NULL DEFAULT '',
    key_hash TEXT NOT NULL,        -- AES-256-GCM 加密（列名历史遗留，实际是密文）
    key_masked TEXT NOT NULL,      -- 脱敏显示
    created_at INTEGER NOT NULL,
    FOREIGN KEY (provider_id) REFERENCES providers(id) ON DELETE CASCADE
);
```

**注意**: `key_hash` 列名是历史遗留，实际存储 AES 密文（不是哈希）。解密失败时**跳过该 key**（记录 warn 日志，不入池），不回退到密文（避免把密文当 key 发给上游，见 ADR-024）。

### 加密算法

**AES-256-GCM**（认证加密，防篡改）

```rust
pub fn encrypt(plaintext: &str, master_key: &[u8]) -> Result<String> {
    let cipher = Aes256Gcm::new_from_slice(master_key)?;
    let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
    let ciphertext = cipher.encrypt(&nonce, plaintext.as_ref())?;
    
    // nonce || ciphertext
    let mut result = nonce.to_vec();
    result.extend_from_slice(&ciphertext);
    Ok(BASE64.encode(&result))
}
```

**主密钥**:
- 位置: `master.key` 文件
- 权限: `0600`（仅所有者读写）
- 生成: 首次启动时随机生成

## 输入契约

### Service Key 认证

```rust
pub fn verify_service_key(
    state: &AppState,
    key: &str
) -> Option<ServiceKeyInfo>
```

**返回**:
- `Some(ServiceKeyInfo)` 认证成功
- `None` 认证失败

### Service Key 创建

```rust
pub fn create_service_key(
    state: &AppState,
    name: &str,
    allowed_models: Vec<String>
) -> Result<(String, String)>  // (id, raw_key)
```

**返回**: 原始 key（仅一次可见）

## 输出契约

### ServiceKeyInfo

```rust
pub struct ServiceKeyInfo {
    pub id: String,
    pub name: String,
    pub key_masked: String,
    pub allowed_models: Vec<String>,
    pub quota_5h: i64,  // 5h 滚动窗口 token 上限，0 = 不设限
    pub quota_7d: i64,  // 7d 滚动窗口 token 上限，0 = 不设限
}
```

### 认证流程

1. 提取 `x-api-key`（Anthropic 端点优先）或 `Authorization: Bearer`（OpenAI 端点优先）头
2. 遍历所有 Service Key
3. 逐条调用 `verify_service_key`
4. 匹配成功返回 `ServiceKeyInfo`
5. 检查 5h/7d 滚动窗口配额（`quota.rs::check_quota`），超限返回 429
6. 检查 `allowed_models` 白名单

## 关键约束

1. **Service Key 不可逆**: Argon2 哈希，无法解密
2. **Provider Key 可逆**: AES-256-GCM 加密，需要解密后发送
3. **主密钥保护**: `master.key` 文件丢失则所有 Provider Key 不可恢复
4. **逐条验证**: Service Key 需要遍历所有记录（性能瓶颈）
5. **白名单**: `allowed_models` 为空时允许所有模型

## 错误处理

| 场景 | 行为 |
|------|------|
| 无 `x-api-key` 头 | 返回 401 |
| Service Key 无效 | 返回 401 |
| 模型不在白名单 | 返回 403 |
| 滚动窗口配额超限 | 返回 429（`quota_error` + `retry-after`） |
| 主密钥文件丢失 | 启动失败 |
| Provider Key 解密失败 | 跳过该 key，记录 error 日志 |

## 实现位置

- `src-tauri/src/crypto/mod.rs` - 加密/哈希算法
- `src-tauri/src/api/proxy/auth.rs` - Service Key 认证
- `src-tauri/src/api/handlers/service_keys.rs` - Service Key CRUD

## 测试要求

1. **单元测试**: Argon2 哈希/验证、AES-256-GCM 加密/解密
2. **集成测试**: Service Key 创建 + 认证、白名单检查
3. **安全测试**: 暴力破解抵抗、彩虹表抵抗

## 完成标准

- [x] Service Key 使用 Argon2id 哈希
- [x] Provider API Key 使用 AES-256-GCM 加密
- [x] 主密钥文件保护（权限 0600）
- [x] 白名单检查（`allowed_models`）
- [x] 5h/7d 滚动窗口配额检查（`quota_5h`/`quota_7d`，超限 429）
- [x] 认证失败返回 401
- [x] 白名单拒绝返回 403
- [x] 通过所有单元测试
