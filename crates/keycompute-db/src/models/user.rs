use crate::DbError;
use chrono::{DateTime, Utc};
use keycompute_types::{AssignableUserRole, UserRole};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

/// 用户模型
#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct User {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub email: String,
    pub name: Option<String>,
    pub role: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// 创建用户请求
#[derive(Debug, Clone, Deserialize)]
pub struct CreateUserRequest {
    pub tenant_id: Uuid,
    pub email: String,
    pub name: Option<String>,
    pub role: Option<UserRole>,
}

/// 更新用户请求
#[derive(Debug, Clone, Deserialize)]
pub struct UpdateUserRequest {
    pub name: Option<String>,
    pub role: Option<AssignableUserRole>,
}

/// 转义 LIKE 通配符，防止用户使用 % 或 _ 造成意外匹配
///
/// 逐字符处理避免多次 replace 导致的重复转义问题。
/// PostgreSQL 默认使用 \ 作为 ESCAPE 字符。
fn escape_like_pattern(s: &str) -> String {
    let mut escaped = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '%' => escaped.push_str(r"\%"),
            '_' => escaped.push_str(r"\_"),
            '\\' => escaped.push_str(r"\\"),
            c => escaped.push(c),
        }
    }
    escaped
}

/// 用户过滤参数
///
/// 持有过滤条件的所有权（包括转义后的搜索字符串），
/// 确保在 QueryBuilder 使用期间数据有效。
struct UserFilter {
    tenant_id: Option<Uuid>,
    role: Option<String>,
    search_escaped: Option<String>,
}

impl UserFilter {
    fn new(tenant_id: Option<Uuid>, role: Option<&str>, search: Option<&str>) -> Self {
        // 预处理搜索词：trim 后为空则视为 None，避免无意义的 SQL LIKE 匹配
        let search_escaped = search
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(escape_like_pattern);
        Self {
            tenant_id,
            role: role.map(String::from),
            search_escaped,
        }
    }
}

/// 向 QueryBuilder 中追加用户过滤 WHERE 子句
///
/// 统一处理租户、角色、搜索三种过滤条件（含 LIKE 通配符转义），
/// 供 `find_all_filtered` 和 `count_all_filtered` 复用。
fn push_user_filter_where<'a>(
    builder: &mut sqlx::QueryBuilder<'a, sqlx::Postgres>,
    filter: &'a UserFilter,
) {
    builder.push(" WHERE (");
    builder.push_bind(filter.tenant_id);
    builder.push("::uuid IS NULL OR tenant_id = ");
    builder.push_bind(filter.tenant_id);
    builder.push(") AND (");
    builder.push_bind(filter.role.as_deref());
    builder.push("::text IS NULL OR role = ");
    builder.push_bind(filter.role.as_deref());
    builder.push(") AND (");
    builder.push_bind(filter.search_escaped.as_deref());
    builder.push("::text IS NULL OR LOWER(email) LIKE '%' || LOWER(");
    builder.push_bind(filter.search_escaped.as_deref());
    builder.push(") || '%' ESCAPE '\\' OR LOWER(COALESCE(name, '')) LIKE '%' || LOWER(");
    builder.push_bind(filter.search_escaped.as_deref());
    builder.push(") || '%' ESCAPE '\\')");
}

impl User {
    /// 创建新用户
    pub async fn create(pool: &sqlx::PgPool, req: &CreateUserRequest) -> Result<User, DbError> {
        let user = sqlx::query_as::<_, User>(
            r#"
            INSERT INTO users (tenant_id, email, name, role)
            VALUES ($1, $2, $3, COALESCE($4, 'user'))
            RETURNING *
            "#,
        )
        .bind(req.tenant_id)
        .bind(&req.email)
        .bind(&req.name)
        .bind(req.role.as_ref().map(|role| role.as_str()))
        .fetch_one(pool)
        .await?;

        Ok(user)
    }

    /// 根据 ID 查找用户
    pub async fn find_by_id(pool: &sqlx::PgPool, id: Uuid) -> Result<Option<User>, DbError> {
        let user = sqlx::query_as::<_, User>("SELECT * FROM users WHERE id = $1")
            .bind(id)
            .fetch_optional(pool)
            .await?;

        Ok(user)
    }

    /// 根据邮箱查找用户
    pub async fn find_by_email(pool: &sqlx::PgPool, email: &str) -> Result<Option<User>, DbError> {
        let user = sqlx::query_as::<_, User>("SELECT * FROM users WHERE email = $1")
            .bind(email)
            .fetch_optional(pool)
            .await?;

        Ok(user)
    }

    /// 查找租户下的所有用户
    pub async fn find_by_tenant(
        pool: &sqlx::PgPool,
        tenant_id: Uuid,
    ) -> Result<Vec<User>, DbError> {
        let users = sqlx::query_as::<_, User>("SELECT * FROM users WHERE tenant_id = $1")
            .bind(tenant_id)
            .fetch_all(pool)
            .await?;

        Ok(users)
    }

    /// 查找所有用户（Admin 全局查询）
    ///
    /// 支持分页，按创建时间倒序排列
    pub async fn find_all(
        pool: &sqlx::PgPool,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<User>, DbError> {
        let users = sqlx::query_as::<_, User>(
            r#"
            SELECT * FROM users
            ORDER BY created_at DESC
            LIMIT $1 OFFSET $2
            "#,
        )
        .bind(limit)
        .bind(offset)
        .fetch_all(pool)
        .await?;

        Ok(users)
    }

    /// 查找所有用户（Admin 带过滤 + 分页）
    ///
    /// 支持租户、角色、搜索过滤，过滤条件下推到 SQL 层以保证分页准确性
    pub async fn find_all_filtered(
        pool: &sqlx::PgPool,
        tenant_id: Option<Uuid>,
        role: Option<&str>,
        search: Option<&str>,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<User>, DbError> {
        let filter = UserFilter::new(tenant_id, role, search);
        let mut builder = sqlx::QueryBuilder::<sqlx::Postgres>::new("SELECT * FROM users");
        push_user_filter_where(&mut builder, &filter);
        builder.push(" ORDER BY created_at DESC LIMIT ");
        builder.push_bind(limit);
        builder.push(" OFFSET ");
        builder.push_bind(offset);

        let users = builder.build_query_as::<User>().fetch_all(pool).await?;
        Ok(users)
    }

    /// 统计过滤后的用户总数
    pub async fn count_all_filtered(
        pool: &sqlx::PgPool,
        tenant_id: Option<Uuid>,
        role: Option<&str>,
        search: Option<&str>,
    ) -> Result<i64, DbError> {
        let filter = UserFilter::new(tenant_id, role, search);
        let mut builder = sqlx::QueryBuilder::<sqlx::Postgres>::new("SELECT COUNT(*) FROM users");
        push_user_filter_where(&mut builder, &filter);

        let count: (i64,) = builder.build_query_as().fetch_one(pool).await?;
        Ok(count.0)
    }

    /// 统计用户总数
    pub async fn count_all(pool: &sqlx::PgPool) -> Result<i64, DbError> {
        let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM users")
            .fetch_one(pool)
            .await?;

        Ok(count.0)
    }

    /// 批量统计租户用户数量
    pub async fn count_by_tenants(
        pool: &sqlx::PgPool,
        tenant_ids: &[Uuid],
    ) -> Result<std::collections::HashMap<Uuid, i64>, DbError> {
        use sqlx::FromRow;

        #[derive(FromRow)]
        struct TenantCount {
            tenant_id: Uuid,
            count: i64,
        }

        let rows: Vec<TenantCount> = sqlx::query_as(
            r#"
            SELECT tenant_id, COUNT(*) as count
            FROM users
            WHERE tenant_id = ANY($1)
            GROUP BY tenant_id
            "#,
        )
        .bind(tenant_ids)
        .fetch_all(pool)
        .await?;

        Ok(rows.into_iter().map(|r| (r.tenant_id, r.count)).collect())
    }

    /// 更新用户
    pub async fn update(
        &self,
        pool: &sqlx::PgPool,
        req: &UpdateUserRequest,
    ) -> Result<User, DbError> {
        let user = sqlx::query_as::<_, User>(
            r#"
            UPDATE users
            SET name = COALESCE($1, name),
                role = COALESCE($2, role),
                updated_at = NOW()
            WHERE id = $3
            RETURNING *
            "#,
        )
        .bind(&req.name)
        .bind(req.role.as_ref().map(|role| role.as_str()))
        .bind(self.id)
        .fetch_one(pool)
        .await?;

        Ok(user)
    }

    /// 删除用户
    pub async fn delete(&self, pool: &sqlx::PgPool) -> Result<(), DbError> {
        sqlx::query("DELETE FROM users WHERE id = $1")
            .bind(self.id)
            .execute(pool)
            .await?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_escape_like_pattern_no_special_chars() {
        let result = escape_like_pattern("hello");
        assert_eq!(result, "hello");
    }

    #[test]
    fn test_escape_like_pattern_percent() {
        let result = escape_like_pattern("50%");
        assert_eq!(result, r"50\%");
    }

    #[test]
    fn test_escape_like_pattern_underscore() {
        let result = escape_like_pattern("user_name");
        assert_eq!(result, r"user\_name");
    }

    #[test]
    fn test_escape_like_pattern_backslash() {
        let result = escape_like_pattern(r"a\b");
        assert_eq!(result, r"a\\b");
    }

    #[test]
    fn test_escape_like_pattern_mixed() {
        let result = escape_like_pattern("100%_test\\case");
        assert_eq!(result, r"100\%\_test\\case");
    }

    #[test]
    fn test_escape_like_pattern_empty() {
        let result = escape_like_pattern("");
        assert_eq!(result, "");
    }
}
