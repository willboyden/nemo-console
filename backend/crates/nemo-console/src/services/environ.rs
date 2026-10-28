//! The process environment as a value (core contract). Python reads `os.environ` directly and its tests monkeypatch
//! it; Rust 2024 makes `std::env::set_var` unsafe (forbidden here), so everything that reads a variable (credential
//! values, the runner's child env, token status) goes through an [`Environ`]: the real environment in production, a
//! fixed map in tests (hermetic: a developer's LITELLM_API_KEY can never reach a test or a live service).

use std::collections::HashMap;
use std::sync::Arc;

#[derive(Clone, Debug, Default)]
pub struct Environ {
    fixed: Option<Arc<HashMap<String, String>>>,
}

impl Environ {
    /// The real process environment.
    #[must_use]
    pub fn process() -> Self {
        Environ { fixed: None }
    }

    /// Exactly these variables and no others.
    #[must_use]
    pub fn fixed(vars: HashMap<String, String>) -> Self {
        Environ {
            fixed: Some(Arc::new(vars)),
        }
    }

    /// Exactly these variables and no others.
    #[must_use]
    pub fn from_pairs(pairs: &[(&str, &str)]) -> Self {
        Environ::fixed(
            pairs
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect(),
        )
    }

    #[must_use]
    pub fn get(&self, name: &str) -> Option<String> {
        match &self.fixed {
            Some(m) => m.get(name).cloned(),
            None => std::env::var(name).ok(),
        }
    }

    #[must_use]
    pub fn vars(&self) -> Vec<(String, String)> {
        match &self.fixed {
            Some(m) => m.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
            None => std::env::vars().collect(),
        }
    }
}
