/// The password policy is shared by account changes and password resets.
/// Keep these checks aligned with `keycompute-auth::PasswordValidator` so the
/// browser does not report a password as valid when the server will reject it.
pub const PASSWORD_MIN_LENGTH: usize = 8;
pub const PASSWORD_SPECIAL_CHARACTERS: &str = "!@#$%^&*()_+-=[]{}|;:',.<>?/~`";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PasswordRules {
    pub length: bool,
    pub uppercase: bool,
    pub lowercase: bool,
    pub digit: bool,
    pub special: bool,
}

/// Evaluate the browser-visible password policy using the same semantics as
/// the server validator (byte length and Unicode numeric characters).
pub fn password_rules(password: &str) -> PasswordRules {
    PasswordRules {
        length: password.len() >= PASSWORD_MIN_LENGTH,
        uppercase: password.chars().any(char::is_uppercase),
        lowercase: password.chars().any(char::is_lowercase),
        digit: password.chars().any(char::is_numeric),
        special: password
            .chars()
            .any(|character| PASSWORD_SPECIAL_CHARACTERS.contains(character)),
    }
}

#[cfg(test)]
mod tests {
    use super::{PASSWORD_MIN_LENGTH, password_rules};

    #[test]
    fn password_rules_match_the_server_policy() {
        let rules = password_rules("Strong1!");
        assert!(rules.length && rules.uppercase && rules.lowercase && rules.digit && rules.special);
        assert!(!password_rules("strong1!").uppercase);
        assert!(!password_rules("STRONG1!").lowercase);
        assert!(!password_rules("Strong!!").digit);
        assert!(!password_rules("Strong1").special);
        assert!(!password_rules("S1!").length);
        assert_eq!(PASSWORD_MIN_LENGTH, 8);
    }
}
