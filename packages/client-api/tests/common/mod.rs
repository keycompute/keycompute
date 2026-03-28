//! 测试工具模块
//!
//! 提供 Mock 服务器和测试辅助函数

use client_api::{ApiClient, ClientConfig};
use wiremock::MockServer;

/// 创建带 Mock 服务器的测试客户端
pub async fn create_test_client() -> (ApiClient, MockServer) {
    let mock_server = MockServer::start().await;
    let config = ClientConfig::new(&mock_server.uri());
    let client = ApiClient::new(config).expect("Failed to create client");
    (client, mock_server)
}

/// 测试用常量
pub mod fixtures {
    pub const TEST_ACCESS_TOKEN: &str = "test_access_token_12345";
    pub const TEST_REFRESH_TOKEN: &str = "test_refresh_token_67890";
    pub const TEST_USER_ID: &str = "user_test_001";
    pub const TEST_EMAIL: &str = "test@example.com";
}
