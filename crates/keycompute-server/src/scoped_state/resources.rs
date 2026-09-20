use super::store::{self, ConversationMutation, ListQuery, Scope};
use super::{require_api, resource_stream};
use crate::{
    error::{ApiError, Result},
    extractors::AuthExtractor,
    state::AppState,
};
use axum::{
    Json,
    extract::{Path, RawQuery, State},
    response::{IntoResponse, Response},
};
use keycompute_types::ModelAccessMode;
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ConversationBody {
    #[serde(default)]
    metadata: Option<Value>,
    #[serde(default)]
    items: Option<Vec<Value>>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ConversationUpdate {
    #[serde(default)]
    metadata: Option<Value>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ItemsBody {
    items: Vec<Value>,
}

fn scope(auth: &AuthExtractor, mode: ModelAccessMode) -> Result<Scope> {
    require_api(auth)?;
    Scope::new(auth, mode)
}

fn pool(state: &AppState) -> Result<&keycompute_db::DbRouter> {
    state
        .pool
        .as_deref()
        .ok_or_else(|| ApiError::ServiceUnavailable("Platform state is unavailable".into()))
}

fn parse_pairs(raw: Option<&str>, allowed: &[&str]) -> Result<Vec<(String, String)>> {
    let mut out = Vec::new();
    for (key, value) in url::form_urlencoded::parse(raw.unwrap_or_default().as_bytes()) {
        if !allowed.iter().any(|name| *name == key) {
            return Err(ApiError::BadRequest(format!(
                "Unknown query parameter: {key}"
            )));
        }
        out.push((key.into_owned(), value.into_owned()));
    }
    Ok(out)
}

fn one_query(raw: Option<&str>, name: &str) -> Result<Option<String>> {
    let pairs: Vec<_> = parse_pairs(raw, &["stream", "starting_after"])?
        .into_iter()
        .filter(|(key, _)| key == name)
        .collect();
    if pairs.len() > 1 {
        return Err(ApiError::BadRequest(format!(
            "{name} may be supplied only once"
        )));
    }
    Ok(pairs.into_iter().next().map(|(_, value)| value))
}

fn bool_query(raw: Option<&str>, name: &str) -> Result<bool> {
    match one_query(raw, name)?.as_deref() {
        None => Ok(false),
        Some("true") => Ok(true),
        Some("false") => Ok(false),
        Some(_) => Err(ApiError::BadRequest(format!(
            "{name} must be true or false"
        ))),
    }
}

fn response_json(record: &store::ResponseRecord) -> Result<Value> {
    if !record.retained() {
        return Err(store::missing());
    }
    Ok(store::public_response(record))
}

async fn response_retrieve(
    state: AppState,
    auth: AuthExtractor,
    mode: ModelAccessMode,
    id: String,
    query: Option<String>,
) -> Result<Response> {
    let scope = scope(&auth, mode)?;
    let streaming = bool_query(query.as_deref(), "stream")?;
    let starting_after = one_query(query.as_deref(), "starting_after")?;
    if streaming {
        let cursor = match starting_after {
            None => None,
            Some(value) => Some(value.parse::<i64>().map_err(|_| {
                ApiError::BadRequest("starting_after must be an event cursor".into())
            })?),
        };
        return resource_stream(state, scope, id, cursor).await;
    }
    if starting_after.is_some() {
        return Err(ApiError::BadRequest(
            "starting_after requires stream=true".into(),
        ));
    }
    let record = store::response(pool(&state)?.write_conn(), scope, &id, false).await?;
    Ok(Json(response_json(&record)?).into_response())
}

async fn response_cancel_or_delete(
    state: AppState,
    auth: AuthExtractor,
    mode: ModelAccessMode,
    id: String,
    delete: bool,
) -> Result<Response> {
    let scope = scope(&auth, mode)?;
    let p = pool(&state)?;
    let before = store::response(p.write_conn(), scope, &id, delete).await?;
    let was_active = before.active();
    let record = store::cancel_or_delete(p, scope, &id, delete).await?;
    if was_active {
        super::cancel_execution(&state, &before).await?;
    }
    if delete {
        return Ok(Json(json!({"id": id, "object": "response", "deleted": true})).into_response());
    }
    Ok(Json(response_json(&record)?).into_response())
}

async fn response_input_items(
    state: AppState,
    auth: AuthExtractor,
    mode: ModelAccessMode,
    id: String,
    query: Option<String>,
) -> Result<Response> {
    let scope = scope(&auth, mode)?;
    let record = store::response(pool(&state)?.write_conn(), scope, &id, false).await?;
    if !record.retained() {
        return Err(store::missing());
    }
    let pairs = parse_pairs(query.as_deref(), &["after", "limit", "order"])?;
    let mut list = ListQuery::default();
    for (key, value) in pairs {
        match key.as_str() {
            "after" => {
                if list.after.replace(value).is_some() {
                    return Err(ApiError::BadRequest(
                        "after may be supplied only once".into(),
                    ));
                }
            }
            "limit" => {
                if list
                    .limit
                    .replace(
                        value
                            .parse()
                            .map_err(|_| ApiError::BadRequest("limit must be an integer".into()))?,
                    )
                    .is_some()
                {
                    return Err(ApiError::BadRequest(
                        "limit may be supplied only once".into(),
                    ));
                }
            }
            "order" => {
                if list.order.replace(value).is_some() {
                    return Err(ApiError::BadRequest(
                        "order may be supplied only once".into(),
                    ));
                }
            }
            _ => unreachable!(),
        }
    }
    Ok(Json(store::list_items(
        &store::response_input_items(&record),
        &list,
    )?)
    .into_response())
}

macro_rules! response_handlers {
    ($retrieve:ident, $delete:ident, $cancel:ident, $items:ident, $mode:ident) => {
        pub async fn $retrieve(
            State(state): State<AppState>,
            auth: AuthExtractor,
            Path(id): Path<String>,
            RawQuery(query): RawQuery,
        ) -> Result<Response> {
            response_retrieve(state, auth, ModelAccessMode::$mode, id, query).await
        }
        pub async fn $delete(
            State(state): State<AppState>,
            auth: AuthExtractor,
            Path(id): Path<String>,
        ) -> Result<Response> {
            response_cancel_or_delete(state, auth, ModelAccessMode::$mode, id, true).await
        }
        pub async fn $cancel(
            State(state): State<AppState>,
            auth: AuthExtractor,
            Path(id): Path<String>,
        ) -> Result<Response> {
            response_cancel_or_delete(state, auth, ModelAccessMode::$mode, id, false).await
        }
        pub async fn $items(
            State(state): State<AppState>,
            auth: AuthExtractor,
            Path(id): Path<String>,
            RawQuery(query): RawQuery,
        ) -> Result<Response> {
            response_input_items(state, auth, ModelAccessMode::$mode, id, query).await
        }
    };
}

response_handlers!(
    passthrough_response_retrieve,
    passthrough_response_delete,
    passthrough_response_cancel,
    passthrough_response_input_items,
    Passthrough
);
response_handlers!(
    node_response_retrieve,
    node_response_delete,
    node_response_cancel,
    node_response_input_items,
    NodeDispatch
);

async fn conversation_create(
    state: AppState,
    auth: AuthExtractor,
    mode: ModelAccessMode,
    Json(body): Json<Value>,
) -> Result<Response> {
    let scope = scope(&auth, mode)?;
    let parsed: ConversationBody = serde_json::from_value(body)
        .map_err(|_| ApiError::BadRequest("Invalid conversation request".into()))?;
    let metadata = store::metadata(parsed.metadata.as_ref())?;
    let items = parsed.items.unwrap_or_default();
    let row = store::create_conversation(pool(&state)?, scope, metadata, items).await?;
    Ok(Json(store::conversation_view(&row)).into_response())
}

async fn conversation_retrieve(
    state: AppState,
    auth: AuthExtractor,
    mode: ModelAccessMode,
    id: String,
) -> Result<Response> {
    let scope = scope(&auth, mode)?;
    let row = store::conversation(pool(&state)?.write_conn(), scope, &id, false).await?;
    Ok(Json(store::conversation_view(&row)).into_response())
}

async fn conversation_update(
    state: AppState,
    auth: AuthExtractor,
    mode: ModelAccessMode,
    id: String,
    Json(body): Json<Value>,
) -> Result<Response> {
    let scope = scope(&auth, mode)?;
    let parsed: ConversationUpdate = serde_json::from_value(body)
        .map_err(|_| ApiError::BadRequest("Invalid conversation update".into()))?;
    let metadata = store::metadata(parsed.metadata.as_ref())?;
    let row = store::mutate_conversation(
        pool(&state)?,
        scope,
        &id,
        ConversationMutation::Metadata(metadata),
    )
    .await?;
    Ok(Json(store::conversation_view(&row)).into_response())
}

async fn conversation_delete(
    state: AppState,
    auth: AuthExtractor,
    mode: ModelAccessMode,
    id: String,
) -> Result<Response> {
    let scope = scope(&auth, mode)?;
    let p = pool(&state)?;
    let prior = store::conversation(p.write_conn(), scope, &id, false).await?;
    let active = if let Some(response_id) = prior.active_response_id.as_deref() {
        store::response(p.write_conn(), scope, response_id, false)
            .await
            .ok()
    } else {
        None
    };
    let response = store::mutate_conversation(p, scope, &id, ConversationMutation::Delete).await?;
    if let Some(record) = active {
        super::cancel_execution(&state, &record).await?;
    }
    let _ = response;
    Ok(Json(json!({"id": id, "object": "conversation", "deleted": true})).into_response())
}

async fn conversation_items_list(
    state: AppState,
    auth: AuthExtractor,
    mode: ModelAccessMode,
    id: String,
    query: Option<String>,
) -> Result<Response> {
    let scope = scope(&auth, mode)?;
    let row = store::conversation(pool(&state)?.write_conn(), scope, &id, false).await?;
    let pairs = parse_pairs(query.as_deref(), &["after", "limit", "order"])?;
    let mut list = ListQuery::default();
    for (key, value) in pairs {
        match key.as_str() {
            "after" => {
                list.after = Some(value);
            }
            "limit" => {
                list.limit = Some(
                    value
                        .parse()
                        .map_err(|_| ApiError::BadRequest("limit must be an integer".into()))?,
                );
            }
            "order" => {
                list.order = Some(value);
            }
            _ => unreachable!(),
        }
    }
    Ok(Json(store::list_items(
        row.items_json.as_array().ok_or_else(store::missing)?,
        &list,
    )?)
    .into_response())
}

async fn conversation_items_create(
    state: AppState,
    auth: AuthExtractor,
    mode: ModelAccessMode,
    id: String,
    Json(body): Json<Value>,
) -> Result<Response> {
    let scope = scope(&auth, mode)?;
    let parsed: ItemsBody = serde_json::from_value(body)
        .map_err(|_| ApiError::BadRequest("Invalid conversation items request".into()))?;
    let row = store::mutate_conversation(
        pool(&state)?,
        scope,
        &id,
        ConversationMutation::Append(parsed.items),
    )
    .await?;
    Ok(Json(store::conversation_view(&row)).into_response())
}

async fn conversation_item_retrieve(
    state: AppState,
    auth: AuthExtractor,
    mode: ModelAccessMode,
    conversation_id: String,
    item_id: String,
) -> Result<Response> {
    let scope = scope(&auth, mode)?;
    let row =
        store::conversation(pool(&state)?.write_conn(), scope, &conversation_id, false).await?;
    let item = row
        .items_json
        .as_array()
        .and_then(|items| items.iter().find(|item| item["id"] == item_id))
        .ok_or_else(store::missing)?;
    let mut body = item["body"].clone();
    body["id"] = item_id.into();
    Ok(Json(body).into_response())
}

async fn conversation_item_delete(
    state: AppState,
    auth: AuthExtractor,
    mode: ModelAccessMode,
    conversation_id: String,
    item_id: String,
) -> Result<Response> {
    let scope = scope(&auth, mode)?;
    let row = store::mutate_conversation(
        pool(&state)?,
        scope,
        &conversation_id,
        ConversationMutation::RemoveItem(item_id.clone()),
    )
    .await?;
    Ok(Json(json!({"id": item_id, "object": "conversation.item", "deleted": true, "conversation": row.id})).into_response())
}

macro_rules! conversation_mode_handlers {
    ($create:ident, $retrieve:ident, $update:ident, $delete:ident, $list:ident, $items_create:ident, $item_retrieve:ident, $item_delete:ident, $mode:ident) => {
        pub async fn $create(
            State(state): State<AppState>,
            auth: AuthExtractor,
            Json(body): Json<Value>,
        ) -> Result<Response> {
            conversation_create(state, auth, ModelAccessMode::$mode, Json(body)).await
        }
        pub async fn $retrieve(
            State(state): State<AppState>,
            auth: AuthExtractor,
            Path(id): Path<String>,
        ) -> Result<Response> {
            conversation_retrieve(state, auth, ModelAccessMode::$mode, id).await
        }
        pub async fn $update(
            State(state): State<AppState>,
            auth: AuthExtractor,
            Path(id): Path<String>,
            Json(body): Json<Value>,
        ) -> Result<Response> {
            conversation_update(state, auth, ModelAccessMode::$mode, id, Json(body)).await
        }
        pub async fn $delete(
            State(state): State<AppState>,
            auth: AuthExtractor,
            Path(id): Path<String>,
        ) -> Result<Response> {
            conversation_delete(state, auth, ModelAccessMode::$mode, id).await
        }
        pub async fn $list(
            State(state): State<AppState>,
            auth: AuthExtractor,
            Path(id): Path<String>,
            RawQuery(query): RawQuery,
        ) -> Result<Response> {
            conversation_items_list(state, auth, ModelAccessMode::$mode, id, query).await
        }
        pub async fn $items_create(
            State(state): State<AppState>,
            auth: AuthExtractor,
            Path(id): Path<String>,
            Json(body): Json<Value>,
        ) -> Result<Response> {
            conversation_items_create(state, auth, ModelAccessMode::$mode, id, Json(body)).await
        }
        pub async fn $item_retrieve(
            State(state): State<AppState>,
            auth: AuthExtractor,
            Path((conversation_id, item_id)): Path<(String, String)>,
        ) -> Result<Response> {
            conversation_item_retrieve(
                state,
                auth,
                ModelAccessMode::$mode,
                conversation_id,
                item_id,
            )
            .await
        }
        pub async fn $item_delete(
            State(state): State<AppState>,
            auth: AuthExtractor,
            Path((conversation_id, item_id)): Path<(String, String)>,
        ) -> Result<Response> {
            conversation_item_delete(
                state,
                auth,
                ModelAccessMode::$mode,
                conversation_id,
                item_id,
            )
            .await
        }
    };
}

conversation_mode_handlers!(
    passthrough_conversation_create,
    passthrough_conversation_retrieve,
    passthrough_conversation_update,
    passthrough_conversation_delete,
    passthrough_conversation_items_list,
    passthrough_conversation_items_create,
    passthrough_conversation_item_retrieve,
    passthrough_conversation_item_delete,
    Passthrough
);
conversation_mode_handlers!(
    node_conversation_create,
    node_conversation_retrieve,
    node_conversation_update,
    node_conversation_delete,
    node_conversation_items_list,
    node_conversation_items_create,
    node_conversation_item_retrieve,
    node_conversation_item_delete,
    NodeDispatch
);
