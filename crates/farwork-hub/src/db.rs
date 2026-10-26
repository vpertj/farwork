//! Postgres pool + migrations.

use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;

use crate::config::Config;

pub async fn connect(config: &Config) -> anyhow::Result<PgPool> {
    let pool = PgPoolOptions::new()
        .max_connections(config.database_max_connections)
        .acquire_timeout(std::time::Duration::from_secs(10))
        .connect(&config.database_url)
        .await?;
    Ok(pool)
}

/// Apply `migrations/` embedded in the binary. Runs on every start; sqlx keeps
/// a ledger so this is a no-op once applied.
pub async fn migrate(pool: &PgPool) -> anyhow::Result<()> {
    sqlx::migrate!("./migrations").run(pool).await?;
    Ok(())
}
