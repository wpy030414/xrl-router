//! local_models 表 CRUD（本地模型：GGUF 权重 + 引擎运行时元数据）。

use crate::types::LocalModel;

impl super::Database {
    pub fn save_local_model(&self, m: &LocalModel) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO local_models (id, repo_id, filename, format, backend, status, model_id, ctx_size, n_gpu_layers, autostart, file_size, local_path, port, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
             ON CONFLICT(id) DO UPDATE SET
                repo_id=excluded.repo_id, filename=excluded.filename, format=excluded.format,
                backend=excluded.backend, status=excluded.status, model_id=excluded.model_id,
                ctx_size=excluded.ctx_size, n_gpu_layers=excluded.n_gpu_layers,
                autostart=excluded.autostart, file_size=excluded.file_size,
                local_path=excluded.local_path, port=excluded.port,
                updated_at=excluded.updated_at",
            rusqlite::params![
                m.id,
                m.repo_id,
                m.filename,
                m.format,
                m.backend,
                m.status,
                m.model_id,
                m.ctx_size,
                m.n_gpu_layers,
                m.autostart,
                m.file_size,
                m.local_path,
                m.port,
                m.created_at,
                m.updated_at,
            ],
        )?;
        Ok(())
    }

    pub fn get_local_model(&self, id: &str) -> anyhow::Result<Option<LocalModel>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, repo_id, filename, format, backend, status, model_id, ctx_size, n_gpu_layers, autostart, file_size, local_path, port, created_at, updated_at
             FROM local_models WHERE id = ?1",
        )?;
        let row = stmt.query_row(rusqlite::params![id], |r| {
            Ok(LocalModel {
                id: r.get(0)?,
                repo_id: r.get(1)?,
                filename: r.get(2)?,
                format: r.get(3)?,
                backend: r.get(4)?,
                status: r.get(5)?,
                model_id: r.get(6)?,
                ctx_size: r.get(7)?,
                n_gpu_layers: r.get(8)?,
                autostart: r.get(9)?,
                file_size: r.get(10)?,
                local_path: r.get(11)?,
                port: r.get(12)?,
                created_at: r.get(13)?,
                updated_at: r.get(14)?,
            })
        });
        match row {
            Ok(m) => Ok(Some(m)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn list_local_models(&self) -> anyhow::Result<Vec<LocalModel>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, repo_id, filename, format, backend, status, model_id, ctx_size, n_gpu_layers, autostart, file_size, local_path, port, created_at, updated_at
             FROM local_models ORDER BY created_at ASC",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(LocalModel {
                id: r.get(0)?,
                repo_id: r.get(1)?,
                filename: r.get(2)?,
                format: r.get(3)?,
                backend: r.get(4)?,
                status: r.get(5)?,
                model_id: r.get(6)?,
                ctx_size: r.get(7)?,
                n_gpu_layers: r.get(8)?,
                autostart: r.get(9)?,
                file_size: r.get(10)?,
                local_path: r.get(11)?,
                port: r.get(12)?,
                created_at: r.get(13)?,
                updated_at: r.get(14)?,
            })
        })?;
        let mut result = Vec::new();
        for r in rows {
            result.push(r?);
        }
        Ok(result)
    }

    pub fn delete_local_model(&self, id: &str) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "DELETE FROM local_models WHERE id = ?1",
            rusqlite::params![id],
        )?;
        Ok(())
    }

    /// display_name（模型对外名）是否已被占（本地模型创建时防撞名）。
    pub fn display_name_taken(&self, name: &str) -> bool {
        let conn = self.conn.lock().unwrap();
        let exists: Result<bool, rusqlite::Error> = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM models WHERE display_name = ?1)",
            rusqlite::params![name],
            |r| r.get(0),
        );
        exists.unwrap_or(false)
    }
}
