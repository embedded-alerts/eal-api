#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_to_development() {
        assert_eq!(
            AppEnvironment::parse(None),
            Ok(AppEnvironment::Development)
        );
        assert_eq!(
            AppEnvironment::parse(Some("  ")),
            Ok(AppEnvironment::Development)
        );
    }

    #[test]
    fn parses_supported_environment_aliases() {
        assert_eq!(
            AppEnvironment::parse(Some("DEV")),
            Ok(AppEnvironment::Development)
        );
        assert_eq!(
            AppEnvironment::parse(Some("test")),
            Ok(AppEnvironment::Test)
        );
        assert_eq!(
            AppEnvironment::parse(Some("Prod")),
            Ok(AppEnvironment::Production)
        );
    }

    #[test]
    fn rejects_unknown_environment() {
        let error = AppEnvironment::parse(Some("staging"))
            .expect_err("unknown environments must fail closed");
        assert!(error.contains("unsupported APP_ENV"));
    }

    #[test]
    fn blocks_production_while_storage_is_process_local() {
        let error = validate_startup_policy(AppEnvironment::Production)
            .expect_err("production must remain blocked");
        assert!(error.contains("process-local"));
    }

    #[test]
    fn permits_development_and_test_for_scaffold_work() {
        assert!(validate_startup_policy(AppEnvironment::Development).is_ok());
        assert!(validate_startup_policy(AppEnvironment::Test).is_ok());
    }

    #[test]
    fn bundled_openapi_document_is_valid_json() {
        let document: serde_json::Value = serde_json::from_str(OPENAPI_DOCUMENT).unwrap();
        assert_eq!(document["openapi"], "3.1.0");
        assert!(document["paths"]["/v1/embeddings/search"].is_object());
        assert!(document["paths"]["/v1/sources/{id}/scan"].is_object());
    }
}
