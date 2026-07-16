use std::collections::HashMap;
use std::sync::Mutex;

use forge::{Capability, InvokeContext, InvokeResult, PluginError, PluginServer};

struct DataSqlitePlugin {
    db: Mutex<rusqlite::Connection>,
}

#[forge::async_trait]
impl forge::Plugin for DataSqlitePlugin {
    fn capabilities(&self) -> Vec<Capability> {
        vec![
            Capability::new("forge.data.query", "1.0.0"),
            Capability::new("forge.data.write", "1.0.0"),
        ]
    }

    async fn invoke(&self, ctx: InvokeContext) -> InvokeResult {
        match ctx.capability.as_str() {
            "forge.data.query" | "forge.data.write" => {
                let text = String::from_utf8_lossy(&ctx.payload);
                let req: HashMap<String, serde_json::Value> =
                    serde_json::from_str(&text).map_err(|e| PluginError {
                        code: "INVALID_PAYLOAD".into(),
                        message: format!("expected JSON with 'sql' field: {e}"),
                        details: HashMap::new(),
                    })?;
                let sql = req
                    .get("sql")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| PluginError {
                        code: "MISSING_SQL".into(),
                        message: "payload must include 'sql' field".into(),
                        details: HashMap::new(),
                    })?;

                // Grab optional params so we can do parameterized queries safely
                let params: Vec<rusqlite::types::Value> = req
                    .get("params")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .map(|v| match v {
                                serde_json::Value::Null => rusqlite::types::Value::Null,
                                serde_json::Value::Number(n) => n
                                    .as_i64()
                                    .map(rusqlite::types::Value::Integer)
                                    .unwrap_or_else(|| {
                                        rusqlite::types::Value::Real(n.as_f64().unwrap_or(0.0))
                                    }),
                                serde_json::Value::String(s) => {
                                    rusqlite::types::Value::Text(s.clone())
                                }
                                serde_json::Value::Bool(b) => {
                                    rusqlite::types::Value::Integer(if *b { 1 } else { 0 })
                                }
                                _ => rusqlite::types::Value::Null,
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                let params_refs: Vec<&dyn rusqlite::types::ToSql> = params
                    .iter()
                    .map(|p| p as &dyn rusqlite::types::ToSql)
                    .collect();

                let db = self.db.lock().map_err(|e| PluginError {
                    code: "INTERNAL".into(),
                    message: format!("lock error: {e}"),
                    details: HashMap::new(),
                })?;

                if ctx.capability == "forge.data.write" {
                    let affected =
                        db.execute(sql, params_refs.as_slice())
                            .map_err(|e| PluginError {
                                code: "SQL_ERROR".into(),
                                message: e.to_string(),
                                details: HashMap::new(),
                            })?;
                    Ok(
                        serde_json::to_vec(&serde_json::json!({"rows_affected": affected}))
                            .unwrap(),
                    )
                } else {
                    let mut stmt = db.prepare(sql).map_err(|e| PluginError {
                        code: "SQL_ERROR".into(),
                        message: e.to_string(),
                        details: HashMap::new(),
                    })?;
                    let col_count = stmt.column_count();
                    let col_names: Vec<String> = (0..col_count)
                        .map(|i| stmt.column_name(i).unwrap_or("").to_string())
                        .collect();
                    let rows: Vec<HashMap<String, serde_json::Value>> = stmt
                        .query_map(params_refs.as_slice(), |row| {
                            let mut map = HashMap::new();
                            for (i, name) in col_names.iter().enumerate() {
                                let val: rusqlite::types::Value = row.get_unwrap(i);
                                let json_val = match val {
                                    rusqlite::types::Value::Null => serde_json::Value::Null,
                                    rusqlite::types::Value::Integer(i) => {
                                        serde_json::Value::Number(i.into())
                                    }
                                    rusqlite::types::Value::Real(f) => {
                                        serde_json::json!(f)
                                    }
                                    rusqlite::types::Value::Text(s) => serde_json::Value::String(s),
                                    rusqlite::types::Value::Blob(b) => serde_json::Value::Array(
                                        b.into_iter()
                                            .map(|x| serde_json::Value::Number((x as u64).into()))
                                            .collect(),
                                    ),
                                };
                                map.insert(name.clone(), json_val);
                            }
                            Ok(map)
                        })
                        .map_err(|e| PluginError {
                            code: "SQL_ERROR".into(),
                            message: e.to_string(),
                            details: HashMap::new(),
                        })?
                        .filter_map(|r| r.ok())
                        .collect();

                    Ok(serde_json::to_vec(&serde_json::json!({"rows": rows})).unwrap())
                }
            }
            other => Err(PluginError::not_found(format!(
                "unknown capability: {other}"
            ))),
        }
    }

    async fn health_check(&self) -> bool {
        self.db.lock().is_ok()
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    if std::env::var("FORGE_LISTEN_ADDR").is_err() {
        std::env::set_var("FORGE_LISTEN_ADDR", "127.0.0.1:50053");
    }

    let db_path =
        std::env::var("FORGE_DATA_DB_PATH").unwrap_or_else(|_| "/tmp/forge-data.db".into());
    let conn = rusqlite::Connection::open(&db_path).expect("failed to open SQLite database");
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS users (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL,
            email TEXT NOT NULL
        );",
    )
    .expect("failed to initialize schema");

    tracing::info!("data-sqlite: database at {}", db_path);

    PluginServer::new(DataSqlitePlugin {
        db: Mutex::new(conn),
    })
    .serve_shape_a()
    .await
}
