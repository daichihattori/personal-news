use axum::{
    extract::{FromRef, FromRequestParts},
    http::{StatusCode, request::Parts},
};
use serde::Deserialize;

use crate::{AppError, AppState, db};

#[derive(Debug, Clone)]
pub struct AuthUser {
    pub user_id: String,
    pub email: String,
    pub display_name: String,
}

#[derive(Deserialize)]
struct GoogleTokenInfo {
    sub: String,
    email: Option<String>,
    name: Option<String>,
}

impl<S> FromRequestParts<S> for AuthUser
where
    S: Send + Sync,
    AppState: FromRef<S>,
{
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let app_state = AppState::from_ref(state);

        let user = if app_state.app_env == "local" {
            AuthUser {
                user_id: app_state.dev_user_id.clone(),
                email: "dev@local".to_string(),
                display_name: "Dev User".to_string(),
            }
        } else {
            let token = parts
                .headers
                .get("Authorization")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.strip_prefix("Bearer "))
                .ok_or_else(|| AppError::unauthorized("missing Authorization header"))?;

            let info = validate_google_token(&app_state.http_client, token)
                .await
                .map_err(|_| AppError::unauthorized("invalid or expired token"))?;

            AuthUser {
                user_id: info.sub,
                email: info.email.unwrap_or_default(),
                display_name: info.name.unwrap_or_else(|| "User".to_string()),
            }
        };

        // Auto-provision user on first request
        let user_record = crate::models::User {
            id: user.user_id.clone(),
            email: user.email.clone(),
            display_name: user.display_name.clone(),
        };
        db::upsert_user(&app_state.db, &user_record)
            .await
            .map_err(|_| AppError::internal("failed to provision user"))?;

        Ok(user)
    }
}

async fn validate_google_token(
    client: &reqwest::Client,
    token: &str,
) -> Result<GoogleTokenInfo, ()> {
    let url = format!("https://oauth2.googleapis.com/tokeninfo?id_token={token}");
    let res = client.get(&url).send().await.map_err(|_| ())?;
    if !res.status().is_success() {
        return Err(());
    }
    res.json::<GoogleTokenInfo>().await.map_err(|_| ())
}

// Needed so Axum can extract AuthUser with a rejection type that impls IntoResponse
impl axum::response::IntoResponse for AppError {
    fn into_response(self) -> axum::response::Response {
        (
            self.status,
            axum::Json(serde_json::json!({ "error": self.message })),
        )
            .into_response()
    }
}

// Re-export so main.rs can use AppError::unauthorized
impl AppError {
    pub fn unauthorized(message: impl Into<String>) -> Self {
        Self { status: StatusCode::UNAUTHORIZED, message: message.into() }
    }
}
