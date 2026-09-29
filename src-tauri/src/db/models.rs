//! models 表 CRUD。

use crate::types::Model;

impl super::Database {
    pub fn save_model(&self, model: &Model) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        // UPSERT 而非 INSERT OR REPLACE：REPLACE 会触发 usage_log 的 FK 删除/报错。
        conn.execute(
            "INSERT INTO models (id, provider_id, model_id, display_name, tier, context_window, max_output_tokens, capabilities, enabled, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
             ON CONFLICT(id) DO UPDATE SET
                provider_id=excluded.provider_id, model_id=excluded.model_id,
                display_name=excluded.display_name, tier=excluded.tier,
                context_window=excluded.context_window, max_output_tokens=excluded.max_output_tokens,
                capabilities=excluded.capabilities, enabled=excluded.enabled,
                updated_at=excluded.updated_at",
            rusqlite::params![
                model.id,
                model.provider_id,
                model.model_id,
                model.display_name,
                model.tier,
                model.context_window,
                model.max_output_tokens,
                model.capabilities,
                model.enabled,
                model.created_at,
                model.updated_at,
            ],
        )?;
        Ok(())
    }

    pub fn get_model(&self, id: &str) -> anyhow::Result<Option<Model>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, provider_id, model_id, display_name, tier, context_window, max_output_tokens, capabilities, enabled, created_at, updated_at FROM models WHERE id = ?1"
        )?;

        let model = stmt.query_row(rusqlite::params![id], |row| {
            Ok(Model {
                id: row.get(0)?,
                provider_id: row.get(1)?,
                model_id: row.get(2)?,
                display_name: row.get(3)?,
                tier: row.get(4)?,
                context_window: row.get(5)?,
                max_output_tokens: row.get(6)?,
                capabilities: row.get(7)?,
                enabled: row.get(8)?,
                created_at: row.get(9)?,
                updated_at: row.get(10)?,
            })
        });

        match model {
            Ok(m) => Ok(Some(m)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn delete_model(&self, id: &str) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        // usage_log 已自包含（V12），不再预清理；直接删除 model 即可。
        conn.execute("DELETE FROM models WHERE id = ?1", rusqlite::params![id])?;
        Ok(())
    }

    /// 该 display_name 是否已被任意 model 使用（组合名冲突校验用）。
    /// display_name 在 models 里非唯一，判「至少一行存在」即可。
    pub fn model_display_name_exists(&self, name: &str) -> anyhow::Result<bool> {
        let conn = self.conn.lock().unwrap();
        let exists: i64 = conn.query_row(
            "SELECT COUNT(*) FROM models WHERE display_name = ?1",
            rusqlite::params![name],
            |row| row.get(0),
        )?;
        Ok(exists > 0)
    }

    /// 清理对 models.display_name 的悬空引用（delete_provider / delete_model
    /// 之后调用；allowed_models 与 combo_members 无外键，是 JSON/TEXT 软引用）：
    /// - combo_members.member_alias：别名不再存在于任何模型 → 删除成员行
    /// - service_keys.allowed_models：JSON 数组里的失效别名 → 过滤后回写
    ///
    /// 注意语义是「别名级」：同名别名只要还有任一供应商在提供就保留引用。
    /// 返回 (清理的 combo 成员行数, 更新的 service_keys 行数)。事务内执行。
    pub fn purge_dangling_model_refs(&self) -> anyhow::Result<(usize, usize)> {
        let conn = self.conn.lock().unwrap();
        let tx = conn.unchecked_transaction()?;

        // 1. combo_members 悬空成员（NOT IN 对空 models 表会匹配全部行——正确语义：
        //    模型全删后所有成员都悬空）
        let purged_members = tx.execute(
            "DELETE FROM combo_members WHERE member_alias NOT IN (SELECT display_name FROM models)",
            [],
        )? as usize;

        // 2. service_keys.allowed_models 逐 key 过滤（先收集现存别名，内存过滤）
        let existing: std::collections::HashSet<String> = {
            let mut stmt = tx.prepare("SELECT DISTINCT display_name FROM models")?;
            let set = stmt
                .query_map([], |row| row.get::<_, String>(0))?
                .filter_map(|r| r.ok())
                .collect();
            set
        };

        let keys_raw: Vec<(String, String)> = {
            let mut stmt =
                tx.prepare("SELECT id, allowed_models FROM service_keys WHERE allowed_models NOT IN ('', '[]')")?;
            let rows = stmt
                .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))?
                .filter_map(|r| r.ok())
                .collect();
            rows
        };

        let mut purged_keys = 0usize;
        for (id, allowed_str) in keys_raw {
            let Ok(list) = serde_json::from_str::<Vec<String>>(&allowed_str) else {
                continue; // 非法 JSON：不动（导出导入兜底场景），避免误清
            };
            let filtered: Vec<String> = list.into_iter().filter(|m| existing.contains(m)).collect();
            let filtered_str = serde_json::to_string(&filtered)?;
            // 仅在有变化时回写，避免无谓的 updated_at 抖动
            if filtered_str != allowed_str {
                tx.execute(
                    "UPDATE service_keys SET allowed_models = ?1, updated_at = ?2 WHERE id = ?3",
                    rusqlite::params![filtered_str, chrono::Utc::now().timestamp(), id],
                )?;
                purged_keys += 1;
            }
        }

        tx.commit()?;
        Ok((purged_members, purged_keys))
    }

    pub fn list_all_models(&self) -> anyhow::Result<Vec<Model>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, provider_id, model_id, display_name, tier, context_window, max_output_tokens, capabilities, enabled, created_at, updated_at FROM models"
        )?;

        let models = stmt.query_map([], |row| {
            Ok(Model {
                id: row.get(0)?,
                provider_id: row.get(1)?,
                model_id: row.get(2)?,
                display_name: row.get(3)?,
                tier: row.get(4)?,
                context_window: row.get(5)?,
                max_output_tokens: row.get(6)?,
                capabilities: row.get(7)?,
                enabled: row.get(8)?,
                created_at: row.get(9)?,
                updated_at: row.get(10)?,
            })
        })?;

        let mut result = Vec::new();
        for model in models {
            result.push(model?);
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use crate::db::Database;
    use crate::types::{Combo, Model};

    fn test_db() -> Database {
        let db = Database::open_in_memory().unwrap();
        db.migrate().unwrap();
        db
    }

    fn add_model(db: &Database, id: &str, provider_id: &str, display_name: &str) {
        // provider 需先存在（models.provider_id 有 FK）；测试里 provider 名与 id 同
        db.save_provider(&crate::types::Provider {
            id: provider_id.to_string(),
            name: format!("P-{}", provider_id),
            kind: crate::types::ProviderKind::ChatCompletions,
            base_url: "https://example.com".to_string(),
            api_path: "/v1/chat/completions".to_string(),
            config: serde_json::json!({}),
            enabled: true,
            created_at: 1,
            updated_at: 1,
            sort_order: 0,
        })
        .unwrap();
        db.save_model(&Model {
            id: id.to_string(),
            provider_id: provider_id.to_string(),
            model_id: format!("real-{}", id),
            display_name: display_name.to_string(),
            tier: "custom".to_string(),
            context_window: 128000,
            max_output_tokens: 4096,
            capabilities: "[\"text\"]".to_string(),
            enabled: true,
            created_at: 1,
            updated_at: 1,
        })
        .unwrap();
    }

    /// Bug 3 回归：删除模型后，悬空的 combo_members 行与 allowed_models 条目
    /// 必须被级联清理；同名别名仍有其他供应商提供时保留。
    #[test]
    fn test_purge_dangling_model_refs() {
        let db = test_db();
        // 两个供应商都提供 "shared"；p1 独有 "solo"
        add_model(&db, "m1", "p1", "shared");
        add_model(&db, "m2", "p2", "shared");
        add_model(&db, "m3", "p1", "solo");

        // 组合：成员 shared / solo / ghost（ghost 本来就不存在）
        db.save_combo(
            &Combo { id: "c1".into(), name: "combo-x".into(), enabled: true, created_at: 1, updated_at: 1 },
            &["shared".into(), "solo".into(), "ghost".into()],
        )
        .unwrap();
        // service key 白名单：shared / solo / ghost
        db.save_service_key("sk1", "K1", "hash", "****", Some(r#"["shared","solo","ghost"]"#))
            .unwrap();
        // 空白名单与非法 JSON 的 key：不应被触碰
        db.save_service_key("sk2", "K2", "hash2", "****2", None).unwrap();
        db.save_service_key("sk3", "K3", "hash3", "****3", Some("not-json")).unwrap();

        // 删除 solo 所在模型（模拟 delete_model）
        db.delete_model("m3").unwrap();
        let (members, keys) = db.purge_dangling_model_refs().unwrap();
        assert_eq!(members, 2, "ghost 与 solo 成员行应被清理");
        assert_eq!(keys, 1, "只有 sk1 的白名单需要回写");

        // 组合成员只剩 shared
        let (_, ms) = db.get_combo("c1").unwrap().unwrap();
        assert_eq!(ms, vec!["shared"], "同名 shared 仍在（p2 提供），solo/ghost 被清");

        // sk1 白名单只剩 shared
        let sks = db.list_service_keys().unwrap();
        let sk1 = sks.iter().find(|k| k["id"] == "sk1").unwrap();
        assert_eq!(sk1["allowed_models"], serde_json::json!(["shared"]));

        // sk2（[]）经 list 显示为空数组；sk3（非法 JSON）不被触碰——
        // list 层会把非法 JSON 兜底显示为 []，验证「不回写」须查 DB 原始值
        let sk2 = sks.iter().find(|k| k["id"] == "sk2").unwrap();
        assert_eq!(sk2["allowed_models"], serde_json::json!([]));
        let raw: String = {
            let conn = db.conn();
            conn.query_row(
                "SELECT allowed_models FROM service_keys WHERE id = 'sk3'",
                [],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert_eq!(raw, "not-json", "非法 JSON 不应被 purge 回写");

        // 幂等：再跑一遍无新增清理
        let (m2, k2) = db.purge_dangling_model_refs().unwrap();
        assert_eq!((m2, k2), (0, 0));

        // 删除全部模型后 shared 也悬空 → 组合成员清空
        db.delete_model("m1").unwrap();
        db.delete_model("m2").unwrap();
        let (m3, k3) = db.purge_dangling_model_refs().unwrap();
        assert_eq!(m3, 1);
        assert_eq!(k3, 1);
        let (_, ms) = db.get_combo("c1").unwrap().unwrap();
        assert!(ms.is_empty());
        let sks = db.list_service_keys().unwrap();
        let sk1 = sks.iter().find(|k| k["id"] == "sk1").unwrap();
        assert_eq!(sk1["allowed_models"], serde_json::json!([]));
    }
}
