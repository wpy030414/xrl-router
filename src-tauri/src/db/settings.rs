//! settings KV 表（轮询指针持久化、应用开关等通用键值）。

impl super::Database {
    /// Get a setting value by key (generic key-value store).
    pub fn get_setting(&self, key: &str) -> anyhow::Result<Option<String>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT value FROM settings WHERE key = ?1")?;
        let row = stmt.query_row(rusqlite::params![key], |row| row.get::<_, String>(0));
        match row {
            Ok(v) => Ok(Some(v)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Set (upsert) a setting value.
    pub fn set_setting(&self, key: &str, value: &str) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            rusqlite::params![key, value],
        )?;
        Ok(())
    }

    /// Export all user data as SQL INSERT statements.
    /// Covers: providers, models, api_keys, service_keys, plugins, usage_log, settings, combos, combo_members.
    pub fn export_sql(&self) -> anyhow::Result<String> {
        let conn = self.conn.lock().unwrap();
        let mut sql = String::from("-- XRL Router export\n");
        sql.push_str(&format!("-- Exported at: {}\n\n", chrono::Utc::now().to_rfc3339()));
        sql.push_str("BEGIN TRANSACTION;\n\n");

        // combos 必须在 combo_members 前导出（FK 顺序）。
        let tables = ["providers", "models", "api_keys", "service_keys", "plugins", "usage_log", "settings", "combos", "combo_members", "conversations"];
        for table in &tables {
            let mut stmt = conn.prepare(&format!("SELECT sql FROM sqlite_master WHERE type='table' AND name='{}'", table))?;
            if let Ok(create_sql) = stmt.query_row([], |row| row.get::<_, String>(0)) {
                sql.push_str(&format!("DROP TABLE IF EXISTS {};\n", table));
                sql.push_str(&create_sql);
                sql.push_str(";\n\n");
            }

            let mut rows = conn.prepare(&format!("SELECT * FROM {}", table))?;
            let col_count = rows.column_count();
            let col_names: Vec<String> = (0..col_count)
                .map(|i| rows.column_name(i).unwrap_or("").to_string())
                .collect();

            let mut mapped = rows.query_map([], |row| {
                let mut vals: Vec<String> = Vec::new();
                for i in 0..col_count {
                    let val = row.get_ref(i)?;
                    let s = match val {
                        rusqlite::types::ValueRef::Null => "NULL".to_string(),
                        rusqlite::types::ValueRef::Integer(v) => v.to_string(),
                        rusqlite::types::ValueRef::Real(v) => v.to_string(),
                        rusqlite::types::ValueRef::Text(v) => {
                            let text = std::str::from_utf8(v).unwrap_or("");
                            format!("'{}'", text.replace('\'', "''"))
                        }
                        rusqlite::types::ValueRef::Blob(_) => "NULL".to_string(),
                    };
                    vals.push(s);
                }
                Ok(vals)
            })?;

            while let Some(Ok(vals)) = mapped.next() {
                sql.push_str(&format!(
                    "INSERT INTO {} ({}) VALUES ({});\n",
                    table,
                    col_names.join(", "),
                    vals.join(", ")
                ));
            }
            sql.push('\n');
        }

        sql.push_str("COMMIT;\n");
        Ok(sql)
    }

    /// Import SQL statements, replacing existing data.
    pub fn import_sql(&self, sql: &str) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute_batch(sql)?;
        Ok(())
    }

    /// Reset all user data (truncate tables), preserving schema_version and settings.
    pub fn reset_all_data(&self) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        // 子表先删：combo_members 在 combos 前。
        let tables = ["usage_log", "conversations", "plugins", "service_keys", "api_keys", "models", "combo_members", "combos", "providers", "settings"];
        for table in &tables {
            conn.execute(&format!("DELETE FROM {}", table), [])?;
        }
        Ok(())
    }
}
