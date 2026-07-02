use crate::DbError;
use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DbBackend, FromQueryResult, Statement};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// 用户推荐关系模型
#[derive(Debug, Clone, FromQueryResult, Serialize, Deserialize)]
pub struct UserReferral {
    pub id: Uuid,
    pub user_id: Uuid,
    pub level1_referrer_id: Option<Uuid>,
    pub level2_referrer_id: Option<Uuid>,
    pub source: Option<String>,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// 创建推荐关系请求
#[derive(Debug, Clone, Deserialize)]
pub struct CreateUserReferralRequest {
    pub user_id: Uuid,
    pub level1_referrer_id: Option<Uuid>,
    pub level2_referrer_id: Option<Uuid>,
    pub source: Option<String>,
}

/// 推荐统计
#[derive(Debug, Clone, Serialize, FromQueryResult)]
pub struct ReferralStats {
    pub total_referrals: i64,
    pub level1_count: i64,
    pub level2_count: i64,
}

impl UserReferral {
    /// 创建推荐关系
    pub async fn create(
        db: &impl ConnectionTrait,
        req: &CreateUserReferralRequest,
    ) -> Result<UserReferral, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"INSERT INTO user_referrals (user_id, level1_referrer_id, level2_referrer_id, source) VALUES ($1, $2, $3, $4) RETURNING *"#,
            [
                req.user_id.into(),
                req.level1_referrer_id.into(),
                req.level2_referrer_id.into(),
                req.source.clone().into(),
            ],
        );
        let referral = UserReferral::find_by_statement(stmt)
            .one(db)
            .await?
            .ok_or_else(|| DbError::Other("create failed to return row".to_string()))?;

        Ok(referral)
    }

    /// 根据用户 ID 查找推荐关系
    pub async fn find_by_user(
        db: &impl ConnectionTrait,
        user_id: Uuid,
    ) -> Result<Option<UserReferral>, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT * FROM user_referrals WHERE user_id = $1",
            [user_id.into()],
        );
        let referral = UserReferral::find_by_statement(stmt).one(db).await?;

        Ok(referral)
    }

    /// 查找一级推荐人推荐的所有用户
    pub async fn find_by_level1_referrer(
        db: &impl ConnectionTrait,
        referrer_id: Uuid,
    ) -> Result<Vec<UserReferral>, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT * FROM user_referrals WHERE level1_referrer_id = $1 ORDER BY created_at DESC",
            [referrer_id.into()],
        );
        let referrals = UserReferral::find_by_statement(stmt).all(db).await?;

        Ok(referrals)
    }

    /// 查找二级推荐人推荐的所有用户
    pub async fn find_by_level2_referrer(
        db: &impl ConnectionTrait,
        referrer_id: Uuid,
    ) -> Result<Vec<UserReferral>, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT * FROM user_referrals WHERE level2_referrer_id = $1 ORDER BY created_at DESC",
            [referrer_id.into()],
        );
        let referrals = UserReferral::find_by_statement(stmt).all(db).await?;

        Ok(referrals)
    }

    /// 获取用户的推荐统计
    pub async fn get_stats_by_referrer(
        db: &impl ConnectionTrait,
        referrer_id: Uuid,
    ) -> Result<ReferralStats, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"SELECT COUNT(*) as total_referrals, COUNT(CASE WHEN level1_referrer_id = $1 THEN 1 END) as level1_count, COUNT(CASE WHEN level2_referrer_id = $1 THEN 1 END) as level2_count FROM user_referrals WHERE level1_referrer_id = $1 OR level2_referrer_id = $1"#,
            [referrer_id.into()],
        );
        let stats = ReferralStats::find_by_statement(stmt)
            .one(db)
            .await?
            .ok_or_else(|| DbError::Other("stats query failed".to_string()))?;

        Ok(stats)
    }

    /// 更新推荐关系状态
    pub async fn update_status(
        &self,
        db: &impl ConnectionTrait,
        status: &str,
    ) -> Result<UserReferral, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE user_referrals SET status = $1, updated_at = NOW() WHERE id = $2 RETURNING *",
            [status.into(), self.id.into()],
        );
        let referral = UserReferral::find_by_statement(stmt)
            .one(db)
            .await?
            .ok_or_else(|| DbError::not_found("UserReferral", self.id.to_string()))?;

        Ok(referral)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_user_referral_request() {
        let user_id = Uuid::new_v4();
        let referrer_id = Uuid::new_v4();

        let req = CreateUserReferralRequest {
            user_id,
            level1_referrer_id: Some(referrer_id),
            level2_referrer_id: None,
            source: Some("invite_code".to_string()),
        };

        assert_eq!(req.user_id, user_id);
        assert_eq!(req.level1_referrer_id, Some(referrer_id));
        assert_eq!(req.source, Some("invite_code".to_string()));
    }
}
