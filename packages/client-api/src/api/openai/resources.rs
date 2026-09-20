//! JSON resource operations retain native bodies and select their URL family explicitly.
use super::OpenAiApi;
use crate::{api::admin::ModelAccessMode, error::Result};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceOrder {
    Asc,
    Desc,
}
#[derive(Debug, Clone, Default)]
pub struct ResourceListQuery {
    pub after: Option<String>,
    pub limit: Option<u32>,
    pub order: Option<ResourceOrder>,
}
impl ResourceListQuery {
    fn suffix(&self) -> String {
        let mut fields = vec![];
        if let Some(after) = &self.after {
            fields.push(format!("after={}", urlencoding::encode(after)));
        }
        if let Some(limit) = self.limit {
            fields.push(format!("limit={limit}"));
        }
        if let Some(order) = self.order {
            fields.push(format!(
                "order={}",
                match order {
                    ResourceOrder::Asc => "asc",
                    ResourceOrder::Desc => "desc",
                }
            ));
        }
        if fields.is_empty() {
            String::new()
        } else {
            format!("?{}", fields.join("&"))
        }
    }
}
fn base(mode: ModelAccessMode) -> &'static str {
    match mode {
        ModelAccessMode::AccountPool => "/v1",
        ModelAccessMode::Passthrough => "/pt/v1",
        ModelAccessMode::NodeDispatch => "/nt/v1",
    }
}
fn resource(mode: ModelAccessMode, collection: &str, id: &str) -> String {
    format!("{}/{collection}/{}", base(mode), urlencoding::encode(id))
}
impl OpenAiApi {
    pub async fn retrieve_response_in_mode(
        &self,
        mode: ModelAccessMode,
        id: &str,
        key: &str,
    ) -> Result<Value> {
        self.client
            .get_json(&resource(mode, "responses", id), key)
            .await
    }
    pub async fn delete_response_in_mode(
        &self,
        mode: ModelAccessMode,
        id: &str,
        key: &str,
    ) -> Result<Value> {
        self.client
            .delete_json(&resource(mode, "responses", id), key)
            .await
    }
    pub async fn cancel_response_in_mode(
        &self,
        mode: ModelAccessMode,
        id: &str,
        key: &str,
    ) -> Result<Value> {
        self.client
            .post_json(
                &format!("{}/cancel", resource(mode, "responses", id)),
                &serde_json::json!({}),
                key,
            )
            .await
    }
    pub async fn response_input_items_in_mode(
        &self,
        mode: ModelAccessMode,
        id: &str,
        query: &ResourceListQuery,
        key: &str,
    ) -> Result<Value> {
        self.client
            .get_json(
                &format!(
                    "{}/input_items{}",
                    resource(mode, "responses", id),
                    query.suffix()
                ),
                key,
            )
            .await
    }
    pub async fn create_conversation_in_mode(
        &self,
        mode: ModelAccessMode,
        body: &Value,
        key: &str,
    ) -> Result<Value> {
        self.client
            .post_json(&format!("{}/conversations", base(mode)), body, key)
            .await
    }
    pub async fn retrieve_conversation_in_mode(
        &self,
        mode: ModelAccessMode,
        id: &str,
        key: &str,
    ) -> Result<Value> {
        self.client
            .get_json(&resource(mode, "conversations", id), key)
            .await
    }
    pub async fn update_conversation_in_mode(
        &self,
        mode: ModelAccessMode,
        id: &str,
        body: &Value,
        key: &str,
    ) -> Result<Value> {
        self.client
            .post_json(&resource(mode, "conversations", id), body, key)
            .await
    }
    pub async fn delete_conversation_in_mode(
        &self,
        mode: ModelAccessMode,
        id: &str,
        key: &str,
    ) -> Result<Value> {
        self.client
            .delete_json(&resource(mode, "conversations", id), key)
            .await
    }
    pub async fn conversation_items_in_mode(
        &self,
        mode: ModelAccessMode,
        id: &str,
        query: &ResourceListQuery,
        key: &str,
    ) -> Result<Value> {
        self.client
            .get_json(
                &format!(
                    "{}/items{}",
                    resource(mode, "conversations", id),
                    query.suffix()
                ),
                key,
            )
            .await
    }
    pub async fn append_conversation_items_in_mode(
        &self,
        mode: ModelAccessMode,
        id: &str,
        body: &Value,
        key: &str,
    ) -> Result<Value> {
        self.client
            .post_json(
                &format!("{}/items", resource(mode, "conversations", id)),
                body,
                key,
            )
            .await
    }
    pub async fn retrieve_conversation_item_in_mode(
        &self,
        mode: ModelAccessMode,
        id: &str,
        item: &str,
        key: &str,
    ) -> Result<Value> {
        self.client
            .get_json(
                &format!(
                    "{}/items/{}",
                    resource(mode, "conversations", id),
                    urlencoding::encode(item)
                ),
                key,
            )
            .await
    }
    pub async fn delete_conversation_item_in_mode(
        &self,
        mode: ModelAccessMode,
        id: &str,
        item: &str,
        key: &str,
    ) -> Result<Value> {
        self.client
            .delete_json(
                &format!(
                    "{}/items/{}",
                    resource(mode, "conversations", id),
                    urlencoding::encode(item)
                ),
                key,
            )
            .await
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn resource_ids_and_list_cursors_are_encoded_as_values() {
        assert_eq!(
            resource(ModelAccessMode::NodeDispatch, "responses", "record:one"),
            "/nt/v1/responses/record%3Aone"
        );
        let q = ResourceListQuery {
            after: Some("item one".into()),
            limit: Some(7),
            order: Some(ResourceOrder::Asc),
        };
        assert_eq!(q.suffix(), "?after=item%20one&limit=7&order=asc");
        assert_eq!(ResourceListQuery::default().suffix(), "");
    }
}
