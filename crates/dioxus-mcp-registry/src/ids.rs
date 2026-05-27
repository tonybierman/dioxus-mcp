//! Typed string keys for registry lookups. The registry is string-keyed (so
//! layouts/themes can be added at runtime without an enum to edit); these
//! newtypes just give call sites a typed handle.

use serde::{Deserialize, Serialize};

macro_rules! str_id {
    ($(#[$m:meta])* $name:ident) => {
        $(#[$m])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
        pub struct $name(pub String);

        impl From<&str> for $name {
            fn from(s: &str) -> Self {
                $name(s.to_string())
            }
        }

        impl From<String> for $name {
            fn from(s: String) -> Self {
                $name(s)
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl std::ops::Deref for $name {
            type Target = str;
            fn deref(&self) -> &str {
                &self.0
            }
        }
    };
}

str_id!(
    /// A layout id (the screen `kind` string).
    LayoutId
);
str_id!(
    /// A theme id.
    ThemeId
);
