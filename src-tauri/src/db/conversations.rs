//! 对话审查：消息捕获、指纹计算、存储与查询。

use super::Database;

impl Database {
    /// Insert or update a conversation record.
    /// If a conversation with the same fingerprint exists, increment request_count
    /// and update messages/updated_at. Otherwise insert a new row.
    pub fn upsert_conversation(
        &self,
        fingerprint: &str,
        service_key_id: &str,
        service_key_name: &str,
        messages_json: &str,
        message_count: i64,
        first_user_message: &str,
        last_message: &str,
        last_message_raw: &str,
        now: i64,
    ) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO conversations (fingerprint, service_key_id, service_key_name, messages, message_count, first_user_message, last_message, last_message_raw, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9)
             ON CONFLICT(fingerprint) DO UPDATE SET
                request_count = request_count + 1,
                messages = excluded.messages,
                message_count = excluded.message_count,
                first_user_message = excluded.first_user_message,
                last_message = excluded.last_message,
                last_message_raw = excluded.last_message_raw,
                updated_at = excluded.updated_at",
            rusqlite::params![
                fingerprint,
                service_key_id,
                service_key_name,
                messages_json,
                message_count,
                first_user_message,
                last_message,
                last_message_raw,
                now,
            ],
        )?;
        Ok(())
    }

    /// List conversations, paginated, newest first.
    /// Optional service_key_id filter.
    pub fn get_conversations_page(
        &self,
        page: i64,
        page_size: i64,
        service_key_id: Option<&str>,
    ) -> anyhow::Result<(i64, Vec<serde_json::Value>)> {
        let conn = self.conn.lock().unwrap();
        let offset = (page - 1) * page_size;

        // Count total
        let total: i64 = if let Some(sk_id) = service_key_id {
            conn.query_row(
                "SELECT COUNT(*) FROM conversations WHERE service_key_id = ?1",
                rusqlite::params![sk_id],
                |row| row.get(0),
            )?
        } else {
            conn.query_row("SELECT COUNT(*) FROM conversations", [], |row| row.get(0))?
        };

        // Fetch page
        let mut data = Vec::new();
        if let Some(sk_id) = service_key_id {
            let mut stmt = conn.prepare(
                "SELECT id, fingerprint, service_key_id, service_key_name, message_count, request_count, first_user_message, last_message, last_message_raw, created_at, updated_at
                 FROM conversations WHERE service_key_id = ?1 ORDER BY updated_at DESC LIMIT ?2 OFFSET ?3",
            )?;
            let rows = stmt.query_map(rusqlite::params![sk_id, page_size, offset], |row| {
                Ok(serde_json::json!({
                    "id": row.get::<_, i64>(0)?,
                    "fingerprint": row.get::<_, String>(1)?,
                    "service_key_id": row.get::<_, String>(2)?,
                    "service_key_name": row.get::<_, String>(3)?,
                    "message_count": row.get::<_, i64>(4)?,
                    "request_count": row.get::<_, i64>(5)?,
                    "first_user_message": row.get::<_, String>(6)?,
                    "last_message": row.get::<_, String>(7)?,
                    "last_message_raw": row.get::<_, String>(8)?,
                    "created_at": row.get::<_, i64>(9)?,
                    "updated_at": row.get::<_, i64>(10)?,
                }))
            })?;
            for r in rows {
                data.push(r?);
            }
        } else {
            let mut stmt = conn.prepare(
                "SELECT id, fingerprint, service_key_id, service_key_name, message_count, request_count, first_user_message, last_message, last_message_raw, created_at, updated_at
                 FROM conversations ORDER BY updated_at DESC LIMIT ?1 OFFSET ?2",
            )?;
            let rows = stmt.query_map(rusqlite::params![page_size, offset], |row| {
                Ok(serde_json::json!({
                    "id": row.get::<_, i64>(0)?,
                    "fingerprint": row.get::<_, String>(1)?,
                    "service_key_id": row.get::<_, String>(2)?,
                    "service_key_name": row.get::<_, String>(3)?,
                    "message_count": row.get::<_, i64>(4)?,
                    "request_count": row.get::<_, i64>(5)?,
                    "first_user_message": row.get::<_, String>(6)?,
                    "last_message": row.get::<_, String>(7)?,
                    "last_message_raw": row.get::<_, String>(8)?,
                    "created_at": row.get::<_, i64>(9)?,
                    "updated_at": row.get::<_, i64>(10)?,
                }))
            })?;
            for r in rows {
                data.push(r?);
            }
        }

        Ok((total, data))
    }

    /// Get a single conversation by ID (for detail view), including full messages JSON.
    pub fn get_conversation(&self, id: i64) -> anyhow::Result<Option<serde_json::Value>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, fingerprint, service_key_id, service_key_name, messages, message_count, request_count, first_user_message, last_message, last_message_raw, created_at, updated_at
             FROM conversations WHERE id = ?1",
        )?;
        let row = stmt.query_row(rusqlite::params![id], |row| {
            let messages_str: String = row.get(4)?;
            let messages: serde_json::Value =
                serde_json::from_str(&messages_str).unwrap_or(serde_json::json!([]));
            Ok(serde_json::json!({
                "id": row.get::<_, i64>(0)?,
                "fingerprint": row.get::<_, String>(1)?,
                "service_key_id": row.get::<_, String>(2)?,
                "service_key_name": row.get::<_, String>(3)?,
                "messages": messages,
                "message_count": row.get::<_, i64>(5)?,
                "request_count": row.get::<_, i64>(6)?,
                "first_user_message": row.get::<_, String>(7)?,
                "last_message": row.get::<_, String>(8)?,
                "last_message_raw": row.get::<_, String>(9)?,
                "created_at": row.get::<_, i64>(10)?,
                "updated_at": row.get::<_, i64>(11)?,
            }))
        });
        match row {
            Ok(v) => Ok(Some(v)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Delete a conversation by ID.
    pub fn delete_conversation(&self, id: i64) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM conversations WHERE id = ?1", rusqlite::params![id])?;
        Ok(())
    }
}
