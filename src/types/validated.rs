use serde::Deserialize;

use crate::error::MemoryError;

// ---------------------------------------------------------------------------
// ValidatedString trait
// ---------------------------------------------------------------------------

/// A string newtype that validates its contents at construction.
///
/// Both [`ScopePath`] and [`MemoryName`] implement this trait. Future validated
/// string types (classification labels, custom field names) should too.
///
/// The trait is `pub(crate)` — it captures the shared pattern without
/// committing it to the public API. Concrete types remain public.
pub(crate) trait ValidatedString: Sized + std::fmt::Display + AsRef<str> {
    /// Validate the raw string. Returns `Ok(())` if valid, or an error
    /// describing why not.
    fn validate(s: &str) -> Result<(), MemoryError>;

    /// Validate `s` and wrap it. Provided automatically via [`validate`].
    fn new(s: impl Into<String>) -> Result<Self, MemoryError> {
        let s = s.into();
        Self::validate(&s)?;
        Ok(Self::wrap(s))
    }

    /// Wrap an already-validated string. Called only from [`new`] — do not
    /// call directly.
    #[doc(hidden)]
    fn wrap(s: String) -> Self;
}

/// Shared `Deserialize` implementation for any [`ValidatedString`].
///
/// Both `ScopePath` and `MemoryName` have identical custom `Deserialize` impls
/// that differ only in which type they construct. This helper eliminates that
/// duplication.
pub(crate) fn deserialize_validated<'de, T, D>(deserializer: D) -> Result<T, D::Error>
where
    T: ValidatedString,
    D: serde::Deserializer<'de>,
{
    let s = String::deserialize(deserializer)?;
    T::new(s).map_err(serde::de::Error::custom)
}
