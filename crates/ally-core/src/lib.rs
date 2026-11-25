//! Shared types and helpers for Dekubi-Ally-X.

/// Version of the Dekubi-Ally-X workspace.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    use super::VERSION;

    #[test]
    fn version_is_not_n_dot_zero() {
        // The updater rejects `N.0` and `N.0.0`.
        let parts: Vec<&str> = VERSION.split('.').collect();
        assert!(
            !(parts.len() >= 2 && parts[1..].iter().all(|p| *p == "0")),
            "{VERSION}"
        );
    }
}
