use std::env;
use dotenvy::dotenv;

#[derive(Clone, Debug)]
pub struct AppConfig {
    pub port: u16,
    pub max_cost_per_query: f64,
    pub price_per_tib: f64,
    pub enforce_mode: bool,
}

impl AppConfig {
    pub fn load_from_env() -> Self {
        dotenv().ok();
        Self::from_process_env()
    }

    /// Reads configuration purely from whatever is already set in the
    /// process environment, without touching a `.env` file. Split out from
    /// `load_from_env` so tests can exercise the parsing logic
    /// deterministically, regardless of whether a local `.env` file exists
    /// on the developer's machine.
    fn from_process_env() -> Self {
        let port = env::var("PORT")
            .unwrap_or_else(|_| "8080".to_string())
            .parse::<u16>()
            .expect("PORT must be a valid number");

        let max_cost_per_query = env::var("BQ_MAX_COST_PER_QUERY")
            .unwrap_or_else(|_| "5.00".to_string())
            .parse::<f64>()
            .expect("BQ_MAX_COST_PER_QUERY must be a valid decimal");

        let price_per_tib = env::var("BQ_PRICE_PER_TIB")
            .unwrap_or_else(|_| "6.25".to_string())
            .parse::<f64>()
            .expect("BQ_PRICE_PER_TIB must be a valid decimal");

        let enforce_mode = env::var("ENFORCE_MODE")
            .unwrap_or_else(|_| "true".to_string())
            .parse::<bool>()
            .unwrap_or(true);

        AppConfig {
            port,
            max_cost_per_query,
            price_per_tib,
            enforce_mode,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    /// Removes every config-related env var so each test starts from a
    /// clean slate, independent of execution order.
    ///
    /// `env::remove_var`/`set_var` are `unsafe` since Rust edition 2024
    /// because mutating process-wide env vars while another thread reads
    /// them is undefined behavior on some platforms. The `#[serial]`
    /// attribute on every test in this module ensures these tests never
    /// run concurrently with each other, which keeps this safe in practice.
    fn clear_env() {
        for var in ["PORT", "BQ_MAX_COST_PER_QUERY", "BQ_PRICE_PER_TIB", "ENFORCE_MODE"] {
            unsafe {
                env::remove_var(var);
            }
        }
    }

    fn set_env(key: &str, value: &str) {
        unsafe {
            env::set_var(key, value);
        }
    }

    #[test]
    #[serial]
    fn uses_documented_defaults_when_env_vars_are_absent() {
        clear_env();

        let config = AppConfig::from_process_env();

        assert_eq!(config.port, 8080);
        assert_eq!(config.max_cost_per_query, 5.00);
        assert_eq!(config.price_per_tib, 6.25);
        assert!(config.enforce_mode);

        clear_env();
    }

    #[test]
    #[serial]
    fn reads_overrides_from_the_environment() {
        clear_env();
        set_env("PORT", "9090");
        set_env("BQ_MAX_COST_PER_QUERY", "12.5");
        set_env("BQ_PRICE_PER_TIB", "7.8125");
        set_env("ENFORCE_MODE", "false");

        let config = AppConfig::from_process_env();

        assert_eq!(config.port, 9090);
        assert_eq!(config.max_cost_per_query, 12.5);
        assert_eq!(config.price_per_tib, 7.8125);
        assert!(!config.enforce_mode);

        clear_env();
    }

    #[test]
    #[serial]
    #[should_panic(expected = "PORT must be a valid number")]
    fn panics_when_port_is_not_numeric() {
        clear_env();
        set_env("PORT", "not-a-port");

        // No cleanup after this: the panic short-circuits execution, but
        // every test calls clear_env() at its own start, so leftover state
        // never leaks into the next test.
        AppConfig::from_process_env();
    }

    #[test]
    #[serial]
    fn falls_back_to_enforced_when_enforce_mode_is_not_a_valid_boolean() {
        clear_env();
        set_env("ENFORCE_MODE", "maybe");

        let config = AppConfig::from_process_env();

        assert!(config.enforce_mode);

        clear_env();
    }
}