//! Shared SQL predicate for readiness and atomic task claims.
//! Arguments are SQL aliases from source code, never request values.
pub fn profile_matches(profile: &str, required: &str) -> String {
    format!(
        r#"(
        {profile} @> jsonb_build_object(
            'version',{required}->'version','model',{required}->'model',
            'operation',{required}->'operation','features',{required}->'features')
        AND (NOT COALESCE(({required}->>'enforce_limits')::boolean,TRUE)
          OR (({profile}->>'max_request_bytes')::bigint >= ({required}->>'request_bytes')::bigint
            AND ({profile}->'max_output_tokens' IS NULL OR {profile}->'max_output_tokens'='null'::jsonb
                OR ({required}->>'output_tokens')::bigint <= ({profile}->>'max_output_tokens')::bigint)))
    )"#
    )
}
#[cfg(test)]
mod tests {
    #[test]
    fn same_predicate_checks_version_model_operation_features_and_limits() {
        let sql = super::profile_matches("p", "r");
        for field in [
            "version",
            "model",
            "operation",
            "features",
            "max_request_bytes",
            "max_output_tokens",
        ] {
            assert!(sql.contains(field));
        }
        assert!(sql.contains("enforce_limits"));
    }
}
