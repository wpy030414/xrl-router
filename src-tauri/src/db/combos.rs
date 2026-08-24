//! combos / combo_members 表 CRUD。
//!
//! 组合别名：多个模型 display_name 按顺序捆绑成新别名（V18）。
//! `combo_members.member_alias` 是 TEXT 软引用（display_name 非唯一，无法建硬 FK），
//! 删除模型不影响组合结构，运行时跳过不可解析成员。

use crate::types::Combo;

impl super::Database {
    /// 列出全部组合（含按 position 排序的成员）。
    pub fn list_combos(&self) -> anyhow::Result<Vec<(Combo, Vec<String>)>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, name, enabled, created_at, updated_at FROM combos ORDER BY created_at ASC, name ASC",
        )?;
        let combos = stmt
            .query_map([], |row| {
                Ok(Combo {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    enabled: row.get(2)?,
                    created_at: row.get(3)?,
                    updated_at: row.get(4)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        let mut stmt = conn.prepare(
            "SELECT member_alias FROM combo_members WHERE combo_id = ?1 ORDER BY position ASC, rowid ASC",
        )?;
        let mut out = Vec::with_capacity(combos.len());
        for combo in combos {
            let members = stmt
                .query_map(rusqlite::params![&combo.id], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            out.push((combo, members));
        }
        Ok(out)
    }

    /// 单个组合 + 成员；None = 不存在。
    pub fn get_combo(&self, id: &str) -> anyhow::Result<Option<(Combo, Vec<String>)>> {
        let conn = self.conn.lock().unwrap();
        let combo = conn
            .query_row(
                "SELECT id, name, enabled, created_at, updated_at FROM combos WHERE id = ?1",
                rusqlite::params![id],
                |row| {
                    Ok(Combo {
                        id: row.get(0)?,
                        name: row.get(1)?,
                        enabled: row.get(2)?,
                        created_at: row.get(3)?,
                        updated_at: row.get(4)?,
                    })
                },
            );
        let combo = match combo {
            Ok(c) => c,
            Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let mut stmt = conn.prepare(
            "SELECT member_alias FROM combo_members WHERE combo_id = ?1 ORDER BY position ASC, rowid ASC",
        )?;
        let members = stmt
            .query_map(rusqlite::params![id], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Some((combo, members)))
    }

    /// 组合名是否已存在（models handler 的 display_name 冲突校验用）。
    pub fn combo_name_exists(&self, name: &str) -> anyhow::Result<bool> {
        let conn = self.conn.lock().unwrap();
        let exists: i64 = conn.query_row(
            "SELECT COUNT(*) FROM combos WHERE name = ?1",
            rusqlite::params![name],
            |row| row.get(0),
        )?;
        Ok(exists > 0)
    }

    /// 事务内 UPSERT 组合头 + 删除重插全部成员（保序）。
    /// 不能用 INSERT OR REPLACE——REPLACE = DELETE + INSERT，会触发
    /// combo_members 的 ON DELETE CASCADE 把成员清空。
    pub fn save_combo(&self, combo: &Combo, members: &[String]) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        let tx = conn.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO combos (id, name, enabled, created_at, updated_at) VALUES (?1,?2,?3,?4,?5)
             ON CONFLICT(id) DO UPDATE SET
                name=excluded.name, enabled=excluded.enabled, updated_at=excluded.updated_at",
            rusqlite::params![
                combo.id,
                combo.name,
                combo.enabled,
                combo.created_at,
                combo.updated_at,
            ],
        )?;
        tx.execute(
            "DELETE FROM combo_members WHERE combo_id = ?1",
            rusqlite::params![combo.id],
        )?;
        for (i, alias) in members.iter().enumerate() {
            tx.execute(
                "INSERT INTO combo_members (id, combo_id, member_alias, position) VALUES (?1,?2,?3,?4)",
                rusqlite::params![uuid::Uuid::new_v4().to_string(), combo.id, alias, i as i64],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// 删除组合（combo_members 经 FK ON DELETE CASCADE 级联，PRAGMA foreign_keys=ON 已启用）。
    pub fn delete_combo(&self, id: &str) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM combos WHERE id = ?1", rusqlite::params![id])?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;

    fn test_db() -> Database {
        let db = Database::open_in_memory().unwrap();
        db.migrate().unwrap();
        db
    }

    fn combo(id: &str, name: &str) -> Combo {
        Combo {
            id: id.to_string(),
            name: name.to_string(),
            enabled: true,
            created_at: 1,
            updated_at: 1,
        }
    }

    /// save 重插成员：第二次保存只保留新列表且 position 干净，无残留旧成员。
    #[test]
    fn test_combo_save_replace_members() {
        let db = test_db();
        db.save_combo(&combo("c1", "all"), &["a".into(), "b".into()]).unwrap();
        db.save_combo(&combo("c1", "all"), &["a".into(), "c".into()]).unwrap();

        let (_, members) = db.get_combo("c1").unwrap().unwrap();
        assert_eq!(members, vec!["a", "c"], "成员应整体替换且保序");
    }

    /// 删除组合 → 成员级联清空。
    #[test]
    fn test_combo_delete_cascades_members() {
        let db = test_db();
        db.save_combo(&combo("c1", "all"), &["a".into(), "b".into()]).unwrap();
        db.delete_combo("c1").unwrap();

        let conn = db.conn();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM combo_members WHERE combo_id='c1'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0, "删除组合必须级联清空成员");
    }

    /// 同名组合保存两次 → UNIQUE(name) 违例（错误码 2067），供 handler 映射 400。
    #[test]
    fn test_combo_name_unique_violation() {
        let db = test_db();
        db.save_combo(&combo("c1", "all"), &["a".into()]).unwrap();
        let err = db.save_combo(&combo("c2", "all"), &["b".into()]).unwrap_err();
        assert_eq!(err.downcast_ref::<rusqlite::Error>().and_then(|e| e.sqlite_error_code()), Some(rusqlite::ErrorCode::ConstraintViolation));
    }

    /// 名字存在性辅助：model_display_name_exists / combo_name_exists 双向正确。
    #[test]
    fn test_name_exists_helpers() {
        let db = test_db();
        db.save_combo(&combo("c1", "combo-x"), &["a".into()]).unwrap();
        assert!(db.combo_name_exists("combo-x").unwrap());
        assert!(!db.combo_name_exists("nope").unwrap());
        assert!(!db.model_display_name_exists("combo-x").unwrap());
        assert!(!db.model_display_name_exists("a").unwrap());
    }
}
