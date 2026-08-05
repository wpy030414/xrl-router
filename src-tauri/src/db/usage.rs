//! usage_log 写入与统计聚合（V12 起统计自包含，不再 JOIN 父表）。

impl super::Database {
    // Statistics methods
    pub fn get_stats(&self) -> anyhow::Result<(i64, i64)> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT
                COALESCE(SUM(prompt_tokens + completion_tokens + cache_read_input_tokens), 0) as total_tokens,
                COUNT(*) as total_requests
             FROM usage_log"
        )?;

        let stats = stmt.query_row([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
            ))
        })?;

        Ok(stats)
    }

    pub fn get_stats_by_provider(&self) -> anyhow::Result<Vec<serde_json::Value>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT
                u.provider_name,
                COUNT(*) as requests,
                COALESCE(SUM(u.prompt_tokens + u.completion_tokens + u.cache_read_input_tokens), 0) as tokens
             FROM usage_log u
             GROUP BY u.provider_id, u.provider_name"
        )?;

        let stats = stmt.query_map([], |row| {
            Ok(serde_json::json!({
                "provider_name": row.get::<_, String>(0)?,
                "requests": row.get::<_, i64>(1)?,
                "tokens": row.get::<_, i64>(2)?,
            }))
        })?;

        let mut result = Vec::new();
        for stat in stats {
            result.push(stat?);
        }
        Ok(result)
    }

    /// 指定 service key 在 5h / 7d 固定窗口内已用的 tokens。
    /// 窗口按 epoch 对齐（`now - now % window_secs`），与 quota.rs 的
    /// `window_reset_ts` 保持一致，确保新窗口开始时用量归零。
    /// 返回 `(used_5h, used_7d)`；无记录时为 0。
    pub fn get_service_key_usage(&self, service_key_id: &str, now: i64) -> anyhow::Result<(i64, i64)> {
        const FIVE_HOURS: i64 = 5 * 3600;
        const SEVEN_DAYS: i64 = 7 * 86400;
        let window_start_5h = now - (now % FIVE_HOURS);
        let window_start_7d = now - (now % SEVEN_DAYS);
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT
                COALESCE(SUM(CASE WHEN timestamp >= ?2 THEN prompt_tokens + completion_tokens + cache_read_input_tokens ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN timestamp >= ?3 THEN prompt_tokens + completion_tokens + cache_read_input_tokens ELSE 0 END), 0)
             FROM usage_log
             WHERE service_key_id = ?1"
        )?;
        let row = stmt.query_row(rusqlite::params![service_key_id, window_start_5h, window_start_7d], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
        })?;
        Ok(row)
    }

    /// Append one row to usage_log. Called once per proxied request (success or failure).
    /// 统计信息自包含：写入时快照 provider_name / model_display_name / key_name / key_masked /
    /// service_key_name / service_key_masked，确保删除父表行后统计不受影响。
    pub fn insert_usage_log(
        &self,
        timestamp: i64,
        provider_id: &str,
        provider_name: &str,
        model_id: &str,
        model_display_name: &str,
        key_id: Option<&str>,
        key_name: &str,
        key_masked: &str,
        service_key_id: Option<&str>,
        service_key_name: &str,
        service_key_masked: &str,
        request_type: &str,
        prompt_tokens: i64,
        completion_tokens: i64,
        latency_ms: i64,
        success: bool,
        error_message: Option<&str>,
        cache_read_input_tokens: i64,
    ) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO usage_log (timestamp, provider_id, provider_name, model_id, model_display_name, key_id, key_name, key_masked, service_key_id, service_key_name, service_key_masked, request_type, prompt_tokens, completion_tokens, latency_ms, success, error_message, cache_read_input_tokens)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)",
            rusqlite::params![
                timestamp,
                provider_id,
                provider_name,
                model_id,
                model_display_name,
                key_id,
                key_name,
                key_masked,
                service_key_id,
                service_key_name,
                service_key_masked,
                request_type,
                prompt_tokens,
                completion_tokens,
                latency_ms,
                success as i32,
                error_message,
                cache_read_input_tokens,
            ],
        )?;
        Ok(())
    }

    /// Per-bucket, per-key token aggregation in [from_ts, to_ts].
    /// `bucket_seconds` controls the time bucket (3600 = hour, 86400 = day).
    /// The bucket label is encoded `h{bucket}` for hourly and `d{bucket}` for daily,
    /// where `bucket = floor(unix_seconds / bucket_seconds)`; the frontend chart axis
    /// matches on the prefix.
    pub fn get_usage_by_day_and_key(
        &self,
        from_ts: i64,
        to_ts: i64,
        bucket_seconds: i64,
        tz_offset: i64,
    ) -> anyhow::Result<Vec<serde_json::Value>> {
        let prefix = if bucket_seconds == 3600 { "h" } else { "d" };
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT
                COALESCE(u.service_key_id, '') AS skid,
                u.service_key_name AS key_name,
                u.service_key_masked AS key_masked,
                CAST((u.timestamp + ?4) / ?3 AS INTEGER) AS bucket,
                SUM(u.prompt_tokens) AS prompt_tokens,
                SUM(u.completion_tokens) AS completion_tokens,
                SUM(u.cache_read_input_tokens) AS cache_read_tokens,
                COUNT(*) AS requests
             FROM usage_log u
             WHERE u.timestamp >= ?1 AND u.timestamp <= ?2
             GROUP BY COALESCE(u.service_key_id, ''), bucket
             ORDER BY bucket, skid",
        )?;

        let rows = stmt.query_map(rusqlite::params![from_ts, to_ts, bucket_seconds, tz_offset], |row| {
            let prompt: i64 = row.get(4)?;
            let completion: i64 = row.get(5)?;
            let cache_read: i64 = row.get(6)?;
            let bucket: i64 = row.get(3)?;
            let key_id: String = row.get(0)?;
            let key_name: String = row.get(1)?;
            let key_masked: String = row.get(2)?;
            // 按「服务密钥」分组的可读标签（客户端调本代理用的密钥）。
            let key_label = if key_id.is_empty() {
                "(未认证)".to_string()
            } else if key_name.is_empty() {
                if key_masked.is_empty() { key_id.clone() } else { key_masked.clone() }
            } else if key_masked.is_empty() {
                key_name.clone()
            } else {
                format!("{} ({})", key_name, key_masked)
            };
            Ok(serde_json::json!({
                "key_id": key_id,
                "key_name": key_name,
                "key_masked": key_masked,
                "key_label": key_label,
                "day": format!("{}{}", prefix, bucket),
                "prompt_tokens": prompt,
                "completion_tokens": completion,
                "cache_read_input_tokens": cache_read,
                "total_tokens": prompt + completion + cache_read,
                "requests": row.get::<_, i64>(7)?,
            }))
        })?;

        let mut result = Vec::new();
        for row in rows {
            result.push(row?);
        }
        Ok(result)
    }

    /// 在 [from_ts, to_ts] 内按模型聚合用量，用于前端「最爱用的模型」磁贴。
    /// 返回 (model_id, display_name, total_tokens, requests)，按请求次数降序，仅取 Top 1。
    pub fn get_usage_by_model(
        &self,
        from_ts: i64,
        to_ts: i64,
    ) -> anyhow::Result<Vec<serde_json::Value>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT
                u.model_id,
                u.model_display_name,
                SUM(u.prompt_tokens) AS prompt_tokens,
                SUM(u.completion_tokens) AS completion_tokens,
                SUM(u.cache_read_input_tokens) AS cache_read_tokens,
                COUNT(*) AS requests
             FROM usage_log u
             WHERE u.timestamp >= ?1 AND u.timestamp <= ?2
             GROUP BY u.model_id
             ORDER BY requests DESC
             LIMIT 1",
        )?;

        let rows = stmt.query_map(rusqlite::params![from_ts, to_ts], |row| {
            let model_id: String = row.get(0)?;
            let model_name: String = row.get(1)?;  // model_display_name
            let prompt: i64 = row.get(2)?;
            let completion: i64 = row.get(3)?;
            let cache_read: i64 = row.get(4)?;
            let requests: i64 = row.get(5)?;
            Ok(serde_json::json!({
                "model_id": model_id,
                "model_name": model_name,
                "prompt_tokens": prompt,
                "completion_tokens": completion,
                "cache_read_input_tokens": cache_read,
                "total_tokens": prompt + completion + cache_read,
                "requests": requests,
            }))
        })?;

        let mut result = Vec::new();
        for row in rows {
            result.push(row?);
        }
        Ok(result)
    }

    /// 分页拉取请求日志，按时间逆序（同秒按 id 逆序，保证稳定排序）。
    /// 返回 (总行数, 当前页行)。page >= 1；越界页返回空 data。
    pub fn get_usage_log_page(
        &self,
        page: i64,
        page_size: i64,
    ) -> anyhow::Result<(i64, Vec<serde_json::Value>)> {
        let conn = self.conn.lock().unwrap();
        let total: i64 = conn.query_row("SELECT COUNT(*) FROM usage_log", [], |row| row.get(0))?;
        let offset = (page - 1).max(0) * page_size;

        let mut stmt = conn.prepare(
            "SELECT
                id, timestamp,
                provider_name, model_display_name,
                service_key_name, service_key_masked,
                key_name, key_masked,
                request_type,
                prompt_tokens, completion_tokens, latency_ms,
                success, error_message
             FROM usage_log
             ORDER BY timestamp DESC, id DESC
             LIMIT ?1 OFFSET ?2",
        )?;

        let rows = stmt.query_map(rusqlite::params![page_size, offset], |row| {
            Ok(serde_json::json!({
                "id": row.get::<_, i64>(0)?,
                "timestamp": row.get::<_, i64>(1)?,
                "provider_name": row.get::<_, String>(2)?,
                "model_display_name": row.get::<_, String>(3)?,
                "service_key_name": row.get::<_, String>(4)?,
                "service_key_masked": row.get::<_, String>(5)?,
                "key_name": row.get::<_, String>(6)?,
                "key_masked": row.get::<_, String>(7)?,
                "request_type": row.get::<_, String>(8)?,
                "prompt_tokens": row.get::<_, i64>(9)?,
                "completion_tokens": row.get::<_, i64>(10)?,
                "latency_ms": row.get::<_, i64>(11)?,
                "success": row.get::<_, i64>(12)? != 0,
                "error_message": row.get::<_, Option<String>>(13)?,
            }))
        })?;

        let mut result = Vec::new();
        for row in rows {
            result.push(row?);
        }
        Ok((total, result))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 分页拉取：时间逆序、总数正确、越界页空、success 字段回传。
    #[test]
    fn test_get_usage_log_page() {
        let db = super::super::Database::open_in_memory().unwrap();
        db.migrate().unwrap();
        // 插 25 行，timestamp 递增（1_000_000_000 + i），逆序分页后第一页应倒序返回
        for i in 0..25i64 {
            db.insert_usage_log(
                1_000_000_000 + i,
                "p1", "P1", "m1", "M1",
                Some("pk1"), "PK", "pk-masked",
                Some("sk1"), "SK", "sk-masked",
                "/v1/messages",
                10, 20, 30, i % 2 == 0, Some("err"), 0,
            )
            .unwrap();
        }

        // 第 1 页：10 条，逆序（最新的 1_000_000_024 在前）
        let (total, page1) = db.get_usage_log_page(1, 10).unwrap();
        assert_eq!(total, 25);
        assert_eq!(page1.len(), 10);
        assert_eq!(page1[0]["timestamp"], 1_000_000_024);
        assert_eq!(page1[9]["timestamp"], 1_000_000_015);
        // i=24 是偶数 → success = true；错误信息透传
        assert_eq!(page1[0]["success"], serde_json::json!(true));
        assert_eq!(page1[0]["error_message"], serde_json::json!("err"));

        // 第 2 页 / 第 3 页：衔接正确
        let (_, page2) = db.get_usage_log_page(2, 10).unwrap();
        assert_eq!(page2[0]["timestamp"], 1_000_000_014);
        let (_, page3) = db.get_usage_log_page(3, 10).unwrap();
        assert_eq!(page3.len(), 5);
        assert_eq!(page3[4]["timestamp"], 1_000_000_000);

        // 越界页：空数据
        let (total4, page4) = db.get_usage_log_page(4, 10).unwrap();
        assert_eq!(total4, 25);
        assert!(page4.is_empty());
    }

    /// 5h / 7d 固定窗口聚合：只有当前窗口内的行被计入，跨窗口后用量归零。
    #[test]
    fn test_get_service_key_usage_windows() {
        let db = super::super::Database::open_in_memory().unwrap();
        db.migrate().unwrap();

        let now = 1_000_000_000i64; // 固定 now，便于控制时间
        let insert = |ts: i64, tokens: i64| {
            db.insert_usage_log(
                ts,
                "p1", "P1", "m1", "M1",
                Some("pk1"), "PK", "pk-masked",
                Some("sk1"), "SK", "sk-masked",
                "/v1/messages",
                tokens, 0, 10, true, None, 0,
            )
            .unwrap();
        };

        // 当前 5h 窗口内（计入 5h + 7d）
        insert(now - 3600, 100);
        // 5h 窗口之前、7d 窗口之内（只计入 7d）
        insert(now - 6 * 3600, 200);
        // 7d 窗口之外（两个窗口都不计入）
        insert(now - 8 * 86400, 400);

        let (used_5h, used_7d) = db.get_service_key_usage("sk1", now).unwrap();
        assert_eq!(used_5h, 100);
        assert_eq!(used_7d, 300);

        // 无记录 key 返回 0
        let (a, b) = db.get_service_key_usage("nobody", now).unwrap();
        assert_eq!((a, b), (0, 0));
    }

    /// 固定窗口语义：进入新窗口后，旧窗口的用量不再计入。
    #[test]
    fn test_get_service_key_usage_fixed_window_reset() {
        let db = super::super::Database::open_in_memory().unwrap();
        db.migrate().unwrap();

        let insert = |ts: i64, tokens: i64| {
            db.insert_usage_log(
                ts,
                "p1", "P1", "m1", "M1",
                Some("pk1"), "PK", "pk-masked",
                Some("sk1"), "SK", "sk-masked",
                "/v1/messages",
                tokens, 0, 10, true, None, 0,
            )
            .unwrap();
        };

        // 窗口边界 = 18000 的倍数。设在 36000 处有一个边界。
        let boundary = 36_000i64;

        // 上一个窗口（timestamp < boundary）插入 500 tokens
        insert(boundary - 100, 500);

        // 新窗口刚开始（boundary + 60），5h 用量应为 0
        let (used_5h, _used_7d) = db.get_service_key_usage("sk1", boundary + 60).unwrap();
        assert_eq!(used_5h, 0);

        // 在新窗口内插入 200 tokens
        insert(boundary + 300, 200);
        let (used_5h, _) = db.get_service_key_usage("sk1", boundary + 600).unwrap();
        assert_eq!(used_5h, 200);
    }
}
