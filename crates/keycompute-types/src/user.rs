//! 用户相关共享类型

use serde::{Deserialize, Serialize};
use std::str::FromStr;

macro_rules! impl_role_enum {
    ($type_name:ident, [$($variant:ident => $value:literal),+ $(,)?]) => {
        impl $type_name {
            /// 返回数据库和接口使用的小写角色值。
            pub const fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $value,)+
                }
            }

            /// 返回所有允许写入的角色值，用于错误提示。
            pub const fn allowed_values() -> &'static [&'static str] {
                &[$($value,)+]
            }

            /// 从原始字符串解析角色，并返回统一的验证错误文案。
            pub fn parse(input: &str) -> crate::Result<Self> {
                input
                    .parse::<Self>()
                    .map_err(crate::KeyComputeError::ValidationError)
            }
        }

        impl FromStr for $type_name {
            type Err = String;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                match value {
                    $($value => Ok(Self::$variant),)+
                    _ => Err(format!(
                        "Invalid role: {}. Allowed roles: {}",
                        value,
                        Self::allowed_values().join(", ")
                    )),
                }
            }
        }

        impl std::fmt::Display for $type_name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl From<$type_name> for String {
            fn from(value: $type_name) -> Self {
                value.as_str().to_string()
            }
        }
    };
}

/// 系统支持的用户角色。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UserRole {
    System,
    Admin,
    User,
}

impl_role_enum!(UserRole, [System => "system", Admin => "admin", User => "user"]);

impl UserRole {
    /// 判断角色是否拥有管理员访问能力。
    pub const fn is_admin(self) -> bool {
        matches!(self, UserRole::System | UserRole::Admin)
    }
}

/// 角色编辑接口允许写入的角色子集。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AssignableUserRole {
    Admin,
    User,
}

impl_role_enum!(AssignableUserRole, [Admin => "admin", User => "user"]);

impl From<AssignableUserRole> for UserRole {
    fn from(value: AssignableUserRole) -> Self {
        match value {
            AssignableUserRole::Admin => UserRole::Admin,
            AssignableUserRole::User => UserRole::User,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{AssignableUserRole, UserRole};

    #[test]
    fn test_user_role_parse() {
        assert_eq!("system".parse::<UserRole>().unwrap(), UserRole::System);
        assert_eq!("admin".parse::<UserRole>().unwrap(), UserRole::Admin);
        assert_eq!("user".parse::<UserRole>().unwrap(), UserRole::User);
    }

    #[test]
    fn test_user_role_invalid_parse() {
        let err = "tenant_admin".parse::<UserRole>().unwrap_err();
        assert!(err.contains("Invalid role"));
        assert!(err.contains("system"));
        assert!(err.contains("admin"));
        assert!(err.contains("user"));
    }

    #[test]
    fn test_user_role_serde() {
        let json = serde_json::to_string(&UserRole::Admin).unwrap();
        assert_eq!(json, "\"admin\"");

        let role: UserRole = serde_json::from_str("\"system\"").unwrap();
        assert_eq!(role, UserRole::System);
    }

    #[test]
    fn test_assignable_user_role_parse() {
        assert_eq!(
            "admin".parse::<AssignableUserRole>().unwrap(),
            AssignableUserRole::Admin
        );
        assert_eq!(
            "user".parse::<AssignableUserRole>().unwrap(),
            AssignableUserRole::User
        );
    }

    #[test]
    fn test_assignable_user_role_invalid_parse() {
        let err = "system".parse::<AssignableUserRole>().unwrap_err();
        assert!(err.contains("Invalid role"));
        assert!(err.contains("admin"));
        assert!(err.contains("user"));
    }

    #[test]
    fn test_assignable_user_role_serde() {
        let json = serde_json::to_string(&AssignableUserRole::Admin).unwrap();
        assert_eq!(json, "\"admin\"");

        let role: AssignableUserRole = serde_json::from_str("\"user\"").unwrap();
        assert_eq!(role, AssignableUserRole::User);
    }

    #[test]
    fn test_assignable_role_to_user_role() {
        let role: UserRole = AssignableUserRole::Admin.into();
        assert_eq!(role, UserRole::Admin);
    }
}
