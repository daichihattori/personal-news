use sqlx::SqlitePool;

use crate::models::{ApiKeyInfo, User};

pub async fn upsert_user(pool: &SqlitePool, user: &User) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO users (id, email, display_name)
         VALUES (?, ?, ?)
         ON CONFLICT(id) DO UPDATE SET
           email        = excluded.email,
           display_name = excluded.display_name,
           updated_at   = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')",
    )
    .bind(&user.id)
    .bind(&user.email)
    .bind(&user.display_name)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn save_api_key(
    pool: &SqlitePool,
    user_id: &str,
    provider: &str,
    api_key: &str,
    key_hint: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO api_keys (user_id, provider, api_key, key_hint)
         VALUES (?, ?, ?, ?)
         ON CONFLICT(user_id, provider) DO UPDATE SET
           api_key    = excluded.api_key,
           key_hint   = excluded.key_hint,
           updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')",
    )
    .bind(user_id)
    .bind(provider)
    .bind(api_key)
    .bind(key_hint)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn get_api_key(
    pool: &SqlitePool,
    user_id: &str,
    provider: &str,
) -> Result<Option<String>, sqlx::Error> {
    let row: Option<(String,)> =
        sqlx::query_as("SELECT api_key FROM api_keys WHERE user_id = ? AND provider = ?")
            .bind(user_id)
            .bind(provider)
            .fetch_optional(pool)
            .await?;
    Ok(row.map(|(v,)| v))
}

pub async fn list_api_keys(
    pool: &SqlitePool,
    user_id: &str,
) -> Result<Vec<ApiKeyInfo>, sqlx::Error> {
    let rows: Vec<(String, String)> =
        sqlx::query_as("SELECT provider, key_hint FROM api_keys WHERE user_id = ?")
            .bind(user_id)
            .fetch_all(pool)
            .await?;
    Ok(rows
        .into_iter()
        .map(|(provider, key_hint)| ApiKeyInfo { provider, configured: true, key_hint })
        .collect())
}

pub async fn delete_api_key(
    pool: &SqlitePool,
    user_id: &str,
    provider: &str,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query("DELETE FROM api_keys WHERE user_id = ? AND provider = ?")
        .bind(user_id)
        .bind(provider)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}
