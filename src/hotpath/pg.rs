//! Process-wide PostgreSQL connection pool for the hot path.
//!
//! Every accelerated request used to open fresh libpq connections
//! (`tokio_postgres::connect`) for auth, project lookup, ACL and per-feature
//! queries. Connection setup dominated latency versus the warm ActiveRecord
//! pool that Puma keeps, which is why some accelerated routes ended up slower
//! than plain proxying. This pool keeps idle connections alive and hands them
//! back through an RAII guard.

use std::collections::HashMap;
use std::ops::Deref;
use std::sync::{Arc, Mutex, OnceLock};

use tokio_postgres::{Client, NoTls};

const MAX_IDLE_PER_POOL: usize = 16;

struct Pool {
    connstr: String,
    idle: Mutex<Vec<Client>>,
}

static POOLS: OnceLock<Mutex<HashMap<String, Arc<Pool>>>> = OnceLock::new();

fn pool_for(connstr: &str) -> Arc<Pool> {
    let pools = POOLS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut map = pools.lock().unwrap();
    map.entry(connstr.to_string())
        .or_insert_with(|| {
            Arc::new(Pool {
                connstr: connstr.to_string(),
                idle: Mutex::new(Vec::new()),
            })
        })
        .clone()
}

/// RAII guard returning its connection to the pool on drop.
///
/// Derefs to [`tokio_postgres::Client`], so existing query calls keep working.
pub struct PgConn {
    pool: Arc<Pool>,
    client: Option<Client>,
}

impl Deref for PgConn {
    type Target = Client;

    fn deref(&self) -> &Client {
        self.client.as_ref().expect("PgConn used after drop")
    }
}

impl Drop for PgConn {
    fn drop(&mut self) {
        let Some(client) = self.client.take() else {
            return;
        };
        if client.is_closed() {
            return;
        }
        let mut idle = self.pool.idle.lock().unwrap();
        if idle.len() < MAX_IDLE_PER_POOL {
            idle.push(client);
        }
    }
}

/// Check out a pooled connection, opening a new one when the pool is empty.
pub async fn get(connstr: &str) -> Result<PgConn, String> {
    let pool = pool_for(connstr);
    loop {
        // Lock is released before the next await: only pop, never hold across await.
        let reused = pool.idle.lock().unwrap().pop();
        match reused {
            Some(client) if !client.is_closed() => {
                return Ok(PgConn {
                    pool,
                    client: Some(client),
                });
            }
            // Closed connection: drop it and try the next idle one.
            Some(_) => continue,
            None => break,
        }
    }
    let (client, connection) = tokio_postgres::connect(&pool.connstr, NoTls)
        .await
        .map_err(|e| e.to_string())?;
    tokio::spawn(async move {
        let _ = connection.await;
    });
    Ok(PgConn {
        pool,
        client: Some(client),
    })
}
