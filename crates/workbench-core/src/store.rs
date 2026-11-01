use anyhow::Result;
use parking_lot::Mutex;
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use std::path::Path;

pub struct Store(Mutex<Connection>);

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        let connection = Connection::open(path)?;
        connection.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON; PRAGMA busy_timeout=5000;
            CREATE TABLE IF NOT EXISTS entities(kind TEXT NOT NULL,id TEXT NOT NULL,data TEXT NOT NULL,PRIMARY KEY(kind,id));
            CREATE TABLE IF NOT EXISTS messages(id TEXT PRIMARY KEY,chat_id TEXT NOT NULL,data TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS events(id INTEGER PRIMARY KEY AUTOINCREMENT,run_id TEXT NOT NULL,chat_id TEXT NOT NULL,kind TEXT NOT NULL,data TEXT NOT NULL,created_at TEXT NOT NULL);
            CREATE INDEX IF NOT EXISTS events_chat ON events(chat_id,id);
            CREATE INDEX IF NOT EXISTS messages_chat ON messages(chat_id);
            CREATE TABLE IF NOT EXISTS checkpoints(chat_id TEXT NOT NULL,path TEXT NOT NULL,before_text TEXT,after_text TEXT NOT NULL,PRIMARY KEY(chat_id,path));
            PRAGMA user_version=1;")?;
        Ok(Self(Mutex::new(connection)))
    }
    pub fn put(&self, kind: &str, value: &Value) -> Result<()> {
        let id = value["id"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Entity requires id"))?;
        self.0.lock().execute("INSERT INTO entities(kind,id,data) VALUES(?1,?2,?3) ON CONFLICT(kind,id) DO UPDATE SET data=excluded.data", params![kind,id,value.to_string()])?;
        Ok(())
    }
    pub fn get(&self, kind: &str, id: &str) -> Result<Value> {
        let text: String = self.0.lock().query_row(
            "SELECT data FROM entities WHERE kind=?1 AND id=?2",
            params![kind, id],
            |r| r.get(0),
        )?;
        Ok(serde_json::from_str(&text)?)
    }
    pub fn all(&self, kind: &str) -> Result<Vec<Value>> {
        let conn = self.0.lock();
        let mut statement =
            conn.prepare("SELECT data FROM entities WHERE kind=?1 ORDER BY rowid")?;
        let rows = statement.query_map([kind], |r| r.get::<_, String>(0))?;
        rows.map(|r| Ok(serde_json::from_str(&r?)?)).collect()
    }
    pub fn delete(&self, kind: &str, id: &str) -> Result<()> {
        self.0.lock().execute(
            "DELETE FROM entities WHERE kind=?1 AND id=?2",
            params![kind, id],
        )?;
        Ok(())
    }
    pub fn message(&self, value: &Value) -> Result<()> {
        self.0.lock().execute(
            "INSERT OR REPLACE INTO messages VALUES(?1,?2,?3)",
            params![
                value["id"].as_str(),
                value["chatId"].as_str(),
                value.to_string()
            ],
        )?;
        Ok(())
    }
    pub fn messages(&self, chat: &str) -> Result<Vec<Value>> {
        let conn = self.0.lock();
        let mut stmt = conn.prepare("SELECT data FROM messages WHERE chat_id=?1 ORDER BY rowid")?;
        stmt.query_map([chat], |r| r.get::<_, String>(0))?
            .map(|r| Ok(serde_json::from_str(&r?)?))
            .collect()
    }
    pub fn delete_chat(&self, chat: &str) -> Result<()> {
        let mut conn = self.0.lock();
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM messages WHERE chat_id=?1", [chat])?;
        tx.execute("DELETE FROM events WHERE chat_id=?1", [chat])?;
        tx.execute("DELETE FROM checkpoints WHERE chat_id=?1", [chat])?;
        tx.execute("DELETE FROM entities WHERE kind='chat' AND id=?1", [chat])?;
        tx.commit()?;
        Ok(())
    }
    pub fn event(&self, run: &str, chat: &str, kind: &str, data: Value) -> Result<i64> {
        let conn = self.0.lock();
        conn.execute(
            "INSERT INTO events(run_id,chat_id,kind,data,created_at) VALUES(?1,?2,?3,?4,?5)",
            params![run, chat, kind, data.to_string(), crate::app::now()],
        )?;
        Ok(conn.last_insert_rowid())
    }
    pub fn events(&self, after: i64, chat: Option<&str>) -> Result<Value> {
        let conn = self.0.lock();
        let mut stmt=conn.prepare("SELECT id,run_id,chat_id,kind,data,created_at FROM events WHERE id>?1 AND (?2 IS NULL OR chat_id=?2) ORDER BY id LIMIT 1000")?;
        let events:Vec<Value>=stmt.query_map(params![after,chat],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,String>(4)?,r.get::<_,String>(5)?)))?.map(|r|{let(id,run,chat,kind,data,date)=r?;Ok(json!({"id":id,"runId":run,"chatId":chat,"kind":kind,"data":serde_json::from_str::<Value>(&data)?,"createdAt":date}))}).collect::<Result<_>>()?;
        let cursor = events
            .last()
            .and_then(|e| e["id"].as_i64())
            .unwrap_or(after);
        Ok(json!({"events":events,"cursor":cursor}))
    }
    pub fn checkpoint(
        &self,
        chat: &str,
        path: &str,
        before: Option<&str>,
        after: &str,
    ) -> Result<()> {
        self.0.lock().execute("INSERT INTO checkpoints VALUES(?1,?2,?3,?4) ON CONFLICT(chat_id,path) DO UPDATE SET after_text=excluded.after_text",params![chat,path,before,after])?;
        Ok(())
    }
    pub fn checkpoint_get(&self, chat: &str, path: &str) -> Result<(Option<String>, String)> {
        Ok(self.0.lock().query_row(
            "SELECT before_text,after_text FROM checkpoints WHERE chat_id=?1 AND path=?2",
            params![chat, path],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?)
    }
    pub fn checkpoint_delete(&self, chat: &str, path: &str) -> Result<()> {
        self.0.lock().execute(
            "DELETE FROM checkpoints WHERE chat_id=?1 AND path=?2",
            params![chat, path],
        )?;
        Ok(())
    }
    pub fn checkpoint_rows(&self) -> Result<Vec<(String, String, Option<String>)>> {
        let conn = self.0.lock();
        let mut statement =
            conn.prepare("SELECT chat_id,path,before_text FROM checkpoints ORDER BY rowid")?;
        Ok(statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }
}
