//! KeyCompute 支付宝支付模块
//!
//! 提供支付宝支付功能的完整实现，包括：
//! - 电脑网站支付 (Page Pay)
//! - 手机网站支付 (WAP Pay)
//! - 异步通知处理
//! - 订单状态同步
//! - 用户余额管理
//!
//! # 架构设计
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────┐
//! │                      PaymentService                          │
//! │  (统一支付服务入口，整合支付宝API和数据库操作)                   │
//! └─────────────────────────────────────────────────────────────┘
//!          │                              │
//!          ▼                              ▼
//! ┌──────────────────┐          ┌──────────────────┐
//! │   AlipayClient    │          │   Database       │
//! │  (支付宝API封装)   │          │  (订单/余额存储)  │
//! └──────────────────┘          └──────────────────┘
//!          │
//!          ▼
//! ┌──────────────────┐
//! │  Signer/Verifier │
//! │  (RSA2签名验签)   │
//! └──────────────────┘
//! ```
//!
//! # 使用示例
//!
//! ```rust,no_run
//! use keycompute_alipay::{PaymentService, AlipayConfig, CreateOrderRequest};
//! use sea_orm::Database;
//! use uuid::Uuid;
//! use rust_decimal::Decimal;
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     // 从环境变量加载配置
//!     let config = AlipayConfig::from_env()?;
//!     
//!     // 创建数据库连接
//!     let db = Database::connect("postgres://localhost/keycompute").await?;
//!     
//!     // 创建支付服务
//!     let service = PaymentService::new(config, db)?;
//!     
//!     // 创建支付订单
//!     let result = service.create_order(CreateOrderRequest {
//!         tenant_id: Uuid::parse_str("00000000-0000-0000-0000-000000000001")?,
//!         user_id: Uuid::parse_str("00000000-0000-0000-0000-000000000001")?,
//!         amount: Decimal::new(100, 0), // 100元
//!         subject: "API调用充值".to_string(),
//!         body: Some("充值100元".to_string()),
//!     }).await?;
//!     
//!     println!("支付URL: {}", result.pay_url);
//!     println!("订单号: {}", result.out_trade_no);
//!     
//!     Ok(())
//! }
//! ```
//!
//! # 环境变量配置
//!
//! | 变量名 | 说明 | 必填 |
//! |--------|------|------|
//! | ALIPAY_APP_ID | 应用ID | 是 |
//! | ALIPAY_PRIVATE_KEY | 应用私钥 | 是 |
//! | ALIPAY_PUBLIC_KEY | 支付宝公钥 | 是 |
//! | ALIPAY_NOTIFY_URL | 异步通知地址 | 是 |
//! | ALIPAY_RETURN_URL | 同步返回地址 | 否 |
//! | ALIPAY_ENV | 环境(sandbox/production) | 否，默认production |
//! | ALIPAY_TIMEOUT_MINUTES | 支付超时时间(分钟) | 否，默认30 |

pub mod client;
pub mod config;
pub mod service;
pub mod sign;

// 重新导出公共接口
pub use client::{AlipayClient, ClientError, PrecreateResponse, QueryResponse};
pub use config::{AlipayConfig, AlipayEnv, ConfigError};
pub use service::{
    CreateOrderRequest, CreateOrderResult, CreateQrOrderResult, NotifyResult, PaymentError,
    PaymentService, SyncResult, UserBalanceInfo,
};
pub use sign::{AlipaySigner, AlipayVerifier, SignError};

// 重新导出数据库模型（方便使用）
pub use keycompute_db::{
    BalanceTransaction, CreatePaymentOrderRequest, PaymentMethod, PaymentOrder, PaymentOrderStatus,
    TransactionType, UserBalance,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_default() {
        let config = AlipayConfig::default();
        assert_eq!(config.sign_type, "RSA2");
        assert_eq!(config.charset, "utf-8");
        assert_eq!(config.version, "1.0");
        assert_eq!(config.timeout_minutes, 30);
    }
}
