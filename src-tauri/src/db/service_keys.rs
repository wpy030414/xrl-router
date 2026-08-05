//! service_keys 表 CRUD（argon2 哈希存储，哈希函数见 `crypto`）。

impl super::Database {
    pub fn save_service_key(&self, id: &str, name: &str, key_hash: &str, key_masked: &str) -> anyhow::Result<()> {
        let now = chrono::Utc::now().timestamp();
        let conn = self.conn.lock().unwrap();
        // UPSERT 而非 INSERT OR REPLACE：REPLACE 会触发 usage_log.service_key_id 的 FK 清理。
        conn.execute(
            "INSERT INTO service_keys (id, name, key_hash, key_masked, total_requests, total_tokens, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, 0, 0, ?5, ?6)
             ON CONFLICT(id) DO UPDATE SET
                name=excluded.name, key_hash=excluded.key_hash,
                key_masked=excluded.key_masked, updated_at=excluded.updated_at",
            rusqlite::params![id, name, key_hash, key_masked, now, now],
        )?;
        Ok(())
    }

    pub fn list_service_keys(&self) -> anyhow::Result<Vec<serde_json::Value>> {
        // 先查基础字段（作用域结束即释放 conn 锁——Mutex 不可重入，下面要调
        // get_service_key_usage 会再次拿锁）。
        let rows = {
            let conn = self.conn.lock().unwrap();
            let mut stmt = conn.prepare(
                "SELECT id, name, key_masked, allowed_models, quota_5h, quota_7d, total_requests, total_tokens, last_used_at, created_at, updated_at FROM service_keys"
            )?;
            let iter = stmt.query_map([], |row| {
                let allowed: String = row.get(3)?;
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    allowed,
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, i64>(7)?,
                    row.get::<_, Option<i64>>(8)?,
                    row.get::<_, i64>(9)?,
                    row.get::<_, i64>(10)?,
                ))
            })?;
            let mut rows = Vec::new();
            for r in iter {
                rows.push(r?);
            }
            rows
        };

        // 逐 key 聚合 5h/7d 窗口用量——与 /v1/user/balance 共用
        // usage::get_service_key_usage，口径单一来源，改一处两处都变。
        let now = chrono::Utc::now().timestamp();
        let mut result = Vec::new();
        for (id, name, key_masked, allowed_str, quota_5h, quota_7d, total_requests, total_tokens, last_used_at, created_at, updated_at) in rows {
            let (used_5h, used_7d) = self.get_service_key_usage(&id, now).unwrap_or((0, 0));
            let allowed: serde_json::Value =
                serde_json::from_str(&allowed_str).unwrap_or(serde_json::json!([]));
            result.push(serde_json::json!({
                "id": id,
                "name": name,
                "key_masked": key_masked,
                "allowed_models": allowed,
                "quota_5h": quota_5h,
                "quota_7d": quota_7d,
                "total_requests": total_requests,
                "total_tokens": total_tokens,
                "last_used_at": last_used_at,
                "created_at": created_at,
                "updated_at": updated_at,
                "used_5h": used_5h,
                "used_7d": used_7d,
            }));
        }
        Ok(result)
    }

    pub fn delete_service_key(&self, id: &str) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        // usage_log 已自包含（V12），不再预清理；直接删除 service_key 即可。
        conn.execute("DELETE FROM service_keys WHERE id = ?1", rusqlite::params![id])?;
        Ok(())
    }

    /// Update a service key's name / allowed_models / token quotas.
    /// Each `Option` field only updates when present (0 is a valid explicit value).
    pub fn update_service_key(
        &self,
        id: &str,
        name: Option<&str>,
        allowed_models: Option<&str>,
        quota_5h: Option<i64>,
        quota_7d: Option<i64>,
    ) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        let now = chrono::Utc::now().timestamp();
        if let Some(n) = name {
            conn.execute(
                "UPDATE service_keys SET name = ?1, updated_at = ?2 WHERE id = ?3",
                rusqlite::params![n, now, id],
            )?;
        }
        if let Some(a) = allowed_models {
            conn.execute(
                "UPDATE service_keys SET allowed_models = ?1, updated_at = ?2 WHERE id = ?3",
                rusqlite::params![a, now, id],
            )?;
        }
        if let Some(q5) = quota_5h {
            conn.execute(
                "UPDATE service_keys SET quota_5h = ?1, updated_at = ?2 WHERE id = ?3",
                rusqlite::params![q5, now, id],
            )?;
        }
        if let Some(q7) = quota_7d {
            conn.execute(
                "UPDATE service_keys SET quota_7d = ?1, updated_at = ?2 WHERE id = ?3",
                rusqlite::params![q7, now, id],
            )?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// list_service_keys 应带回 5h/7d 固定窗口已用量（与 get_service_key_usage 同口径）。
    #[test]
    fn test_list_service_keys_includes_window_usage() {
        let db = crate::db::Database::open_in_memory().unwrap();
        db.migrate().unwrap();

        db.save_service_key("sk1", "测试", "hash", "mask").unwrap();
        let now = chrono::Utc::now().timestamp();
        // 5h 内 100；5h~7d 之间 200；7d 外 400
        for (ts, tokens) in [
            (now - 3600, 100),
            (now - 6 * 3600, 200),
            (now - 8 * 86400, 400),
        ] {
            db.insert_usage_log(
                ts, "p1", "P1", "m1", "M1",
                Some("pk1"), "PK", "pk-masked",
                Some("sk1"), "测试", "mask",
                "/v1/messages", tokens, 0, 10, true, None, 0,
            )
            .unwrap();
        }

        let keys = db.list_service_keys().unwrap();
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0]["used_5h"], 100, "5h 窗口应只计 5h 内的用量");
        assert_eq!(keys[0]["used_7d"], 300, "7d 窗口应计 7d 内（含 5h 内）用量");
        // 无用量 key 应为 0 而非 null
        db.save_service_key("sk2", "空", "hash2", "mask2").unwrap();
        let keys = db.list_service_keys().unwrap();
        let sk2 = keys.iter().find(|k| k["id"] == "sk2").unwrap();
        assert_eq!(sk2["used_5h"], 0);
        assert_eq!(sk2["used_7d"], 0);
    }
}
