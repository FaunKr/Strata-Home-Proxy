//! The saved list of models: Strata's last answer to `GET /v1/models`.
//! Apps ask for the list first; while Strata is not ready (the PC asleep or
//! waking, gaming mode, a tune, a run over all models) they get this copy at
//! once and send their real request, which wakes the PC. Kept in `DATA_DIR`,
//! one for all keys, since Strata's list does not depend on the key.

use std::{path::PathBuf, sync::Arc, sync::Mutex};

use axum::http::Method;
use serde::{Deserialize, Serialize};

use crate::state::AppState;

const MODELS_FILE: &str = "models.json";

/// The two paths Strata answers the list on (`serve/server.py`).
const LIST_PATHS: [&str; 2] = ["/v1/models", "/models"];

/// Whether a request asks for the list of models; the query does not count.
pub fn is_list(method: &Method, path_and_query: &str) -> bool {
    let path = path_and_query.split('?').next().unwrap_or_default();
    method == Method::GET && LIST_PATHS.contains(&path)
}

/// A list as Strata sent it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedList {
    pub content_type: String,
    pub body: String,
}

pub struct SavedModels {
    data_dir: Option<PathBuf>,
    list: Mutex<Option<SavedList>>,
}

impl SavedModels {
    pub fn new(data_dir: Option<PathBuf>) -> Self {
        let list = data_dir
            .as_ref()
            .and_then(|dir| std::fs::read_to_string(dir.join(MODELS_FILE)).ok())
            .and_then(|text| serde_json::from_str(&text).ok());
        Self {
            data_dir,
            list: Mutex::new(list),
        }
    }

    pub fn get(&self) -> Option<SavedList> {
        self.list.lock().unwrap().clone()
    }

    /// Takes a `200` answer of Strata's to the list as the new copy, unless
    /// it lists no model: Strata lists none while no model is loaded and it
    /// does not unload on idle, and the copy is for the time it does not
    /// answer at all.
    pub fn offer(&self, content_type: Option<&str>, body: &[u8]) {
        if !lists_a_model(body) {
            return;
        }
        let Ok(body) = std::str::from_utf8(body) else {
            return;
        };
        let list = SavedList {
            content_type: content_type.unwrap_or("application/json").to_string(),
            body: body.to_string(),
        };
        let mut held = self.list.lock().unwrap();
        if held.as_ref() == Some(&list) {
            return;
        }
        if let Some(dir) = &self.data_dir {
            let json = serde_json::to_vec(&list).expect("serializes");
            if let Err(e) = crate::files::write_private(dir, MODELS_FILE, &json) {
                tracing::warn!("List of models not saved: {e}");
            }
        }
        *held = Some(list);
    }
}

fn lists_a_model(body: &[u8]) -> bool {
    serde_json::from_slice::<serde_json::Value>(body)
        .ok()
        .and_then(|v| Some(!v.get("data")?.as_array()?.is_empty()))
        .unwrap_or(false)
}

/// Asks Strata for the list after a wake-up, so the copy is current even
/// when no app asked for it while the PC was awake.
pub async fn fetch(state: Arc<AppState>) {
    let Some(strata) = state.strata() else {
        return;
    };
    match strata.models().await {
        Ok((content_type, body)) => state.models.offer(content_type.as_deref(), &body),
        Err(e) => tracing::debug!("List of models not fetched: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_get_of_the_list_counts() {
        assert!(is_list(&Method::GET, "/v1/models"));
        assert!(is_list(&Method::GET, "/models?x=1"));
        assert!(!is_list(&Method::POST, "/v1/models"));
        assert!(!is_list(&Method::GET, "/v1/models/Qwen3"));
        assert!(!is_list(&Method::GET, "/v1/modelsx"));
    }

    #[test]
    fn a_list_without_models_keeps_the_copy() {
        let models = SavedModels::new(None);
        let list = br#"{"object": "list", "data": [{"id": "Qwen3"}]}"#;
        models.offer(None, list);
        models.offer(None, br#"{"object": "list", "data": []}"#);
        models.offer(None, b"not json");
        let saved = models.get().unwrap();
        assert_eq!(saved.body.as_bytes(), list);
        assert_eq!(saved.content_type, "application/json");
    }
}
