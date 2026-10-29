//! Locale negotiation and message catalogs.
//!
//! Player-facing text is addressed by stable [`MessageKey`] values and resolved
//! through [`CatalogSet`]. Brazilian Portuguese is the default and source
//! locale, as described in `docs/internationalization.md`.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::str::FromStr;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::error::{EngineError, ErrorCode};

/// The default and source locale.
pub const DEFAULT_LOCALE: &str = "pt-BR";

/// A canonical BCP 47 locale tag.
///
/// Input is normalized to `language[-Script][-REGION][-variant...]`, with a
/// lowercase language, title-case script, and uppercase region.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Locale(String);

impl Locale {
    /// Parse and canonicalize a locale tag.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::InvalidRequest`] when the tag is empty, too long,
    /// or contains a subtag that is not 2–8 ASCII alphanumerics.
    pub fn parse(input: &str) -> Result<Self, EngineError> {
        let normalized = input.trim().replace('_', "-");
        if normalized.is_empty() || normalized.len() > 64 {
            return Err(invalid_locale(input, "locale tag is empty or too long"));
        }

        let mut parts = normalized.split('-');
        let Some(language) = parts.next() else {
            return Err(invalid_locale(input, "locale tag has no language"));
        };
        if !(2..=8).contains(&language.len()) || !language.chars().all(|c| c.is_ascii_alphabetic())
        {
            return Err(invalid_locale(input, "invalid language subtag"));
        }

        let mut canonical = language.to_ascii_lowercase();
        for part in parts {
            if part.is_empty() || part.len() > 8 || !part.chars().all(|c| c.is_ascii_alphanumeric())
            {
                return Err(invalid_locale(input, "invalid locale subtag"));
            }
            canonical.push('-');
            canonical.push_str(&canonical_subtag(part));
        }

        Ok(Self(canonical))
    }

    /// The canonical tag.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The lowercase language subtag.
    #[must_use]
    pub fn language(&self) -> &str {
        self.0.split('-').next().unwrap_or(&self.0)
    }

    /// Fallback locales in resolution order: exact tag, language, then the
    /// project default (`pt-BR`).
    #[must_use]
    pub fn fallbacks(&self) -> Vec<Self> {
        let mut chain = vec![self.clone()];
        let language_locale = Self(self.language().to_string());
        if !chain.contains(&language_locale) {
            chain.push(language_locale);
        }
        let default = Self::default();
        if !chain.contains(&default) {
            chain.push(default);
        }
        chain
    }
}

impl Default for Locale {
    fn default() -> Self {
        Self(DEFAULT_LOCALE.to_string())
    }
}

impl fmt::Display for Locale {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for Locale {
    type Err = EngineError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

impl TryFrom<String> for Locale {
    type Error = EngineError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value)
    }
}

impl From<Locale> for String {
    fn from(value: Locale) -> Self {
        value.0
    }
}

fn canonical_subtag(part: &str) -> String {
    if part.len() == 2 && part.chars().all(|c| c.is_ascii_alphabetic()) {
        return part.to_ascii_uppercase();
    }
    if part.len() == 4 && part.chars().all(|c| c.is_ascii_alphabetic()) {
        let mut chars = part.chars();
        let first = chars
            .next()
            .map(|c| c.to_ascii_uppercase())
            .unwrap_or_default();
        let rest = chars.as_str().to_ascii_lowercase();
        return format!("{first}{rest}");
    }
    part.to_ascii_lowercase()
}

fn invalid_locale(input: &str, reason: &str) -> EngineError {
    EngineError::new(ErrorCode::InvalidRequest, "invalid locale tag")
        .with_detail("locale", input.to_string())
        .with_detail("reason", reason)
}

/// A stable, namespaced message key such as `entity.door.wooden.name`.
///
/// Keys are lowercase and may contain ASCII letters, digits, dots, underscores,
/// and hyphens. They never contain whitespace or placeholders.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct MessageKey(String);

impl MessageKey {
    /// Validate and create a message key.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::InvalidRequest`] for empty, oversized, uppercase,
    /// or malformed keys.
    pub fn parse(input: &str) -> Result<Self, EngineError> {
        let valid = !input.is_empty()
            && input.len() <= 160
            && !input.starts_with('.')
            && !input.ends_with('.')
            && !input.contains("..")
            && input.chars().all(|c| {
                c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-')
            });
        if !valid {
            return Err(
                EngineError::new(ErrorCode::InvalidRequest, "invalid message key")
                    .with_detail("key", input.to_string()),
            );
        }
        Ok(Self(input.to_string()))
    }

    /// The canonical key.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for MessageKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for MessageKey {
    type Err = EngineError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

impl TryFrom<String> for MessageKey {
    type Error = EngineError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value)
    }
}

impl From<MessageKey> for String {
    fn from(value: MessageKey) -> Self {
        value.0
    }
}

/// Extract named placeholders such as `quantity` from `{quantity}`.
///
/// Invalid braces are ignored so that literal punctuation cannot fail a load.
#[must_use]
pub fn placeholders(text: &str) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    let mut rest = text;
    while let Some(start) = rest.find('{') {
        let after = &rest[start + 1..];
        let Some(end) = after.find('}') else {
            break;
        };
        let name = &after[..end];
        if is_placeholder_name(name) {
            found.insert(name.to_string());
        }
        rest = &after[end + 1..];
    }
    found
}

fn is_placeholder_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 40
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// Verify that a translation preserves exactly the source placeholders.
///
/// # Errors
///
/// Returns [`ErrorCode::ContentInvalid`] with the missing and extra placeholder
/// names when the sets differ.
pub fn validate_placeholders(source: &str, translation: &str) -> Result<(), EngineError> {
    let expected = placeholders(source);
    let actual = placeholders(translation);
    if expected == actual {
        return Ok(());
    }
    let missing = expected
        .difference(&actual)
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(",");
    let extra = actual
        .difference(&expected)
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(",");
    Err(EngineError::new(
        ErrorCode::ContentInvalid,
        "translation placeholders do not match the source",
    )
    .with_detail("missing", missing)
    .with_detail("extra", extra))
}

/// One locale's messages.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageCatalog {
    locale: Locale,
    messages: BTreeMap<MessageKey, String>,
}

impl MessageCatalog {
    /// Create an empty catalog for a locale.
    #[must_use]
    pub fn new(locale: Locale) -> Self {
        Self {
            locale,
            messages: BTreeMap::new(),
        }
    }

    /// The catalog's locale.
    #[must_use]
    pub fn locale(&self) -> &Locale {
        &self.locale
    }

    /// Add a message.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::Conflict`] if the key already exists and
    /// [`ErrorCode::ContentInvalid`] if the text is empty.
    pub fn insert(&mut self, key: MessageKey, text: impl Into<String>) -> Result<(), EngineError> {
        let text = text.into();
        if text.trim().is_empty() {
            return Err(
                EngineError::new(ErrorCode::ContentInvalid, "message text is empty")
                    .with_detail("key", key.to_string())
                    .with_detail("locale", self.locale.to_string()),
            );
        }
        if self.messages.contains_key(&key) {
            return Err(
                EngineError::new(ErrorCode::Conflict, "duplicate message key")
                    .with_detail("key", key.to_string())
                    .with_detail("locale", self.locale.to_string()),
            );
        }
        self.messages.insert(key, text);
        Ok(())
    }

    /// Look up a message in this catalog only.
    #[must_use]
    pub fn get(&self, key: &MessageKey) -> Option<&str> {
        self.messages.get(key).map(String::as_str)
    }

    /// Number of messages.
    #[must_use]
    pub fn len(&self) -> usize {
        self.messages.len()
    }

    /// Whether the catalog has no messages.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }

    /// Iterate over messages in stable key order.
    pub fn iter(&self) -> impl Iterator<Item = (&MessageKey, &str)> {
        self.messages.iter().map(|(key, text)| (key, text.as_str()))
    }
}

/// A message found in a catalog, including which locale supplied it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedMessage<'a> {
    /// Rendered text.
    pub text: &'a str,
    /// Catalog that contained the key.
    pub locale: &'a Locale,
    /// Whether a fallback locale supplied the text.
    pub fallback: bool,
}

/// All loaded catalogs, keyed by locale.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogSet {
    catalogs: BTreeMap<Locale, MessageCatalog>,
}

/// Maximum number of configured locale labels in missing-key diagnostics.
pub const MAX_DIAGNOSTIC_LOCALES: usize = 64;

/// Process-local counts of resolutions that did not find the key in the
/// requested catalog, including unsuccessful resolutions. No keys are stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalizationSnapshot {
    /// Counts for explicitly configured, canonical locales, in locale order.
    pub locales: BTreeMap<Locale, u64>,
    /// Count for all other requested locales, without a locale label.
    pub other: u64,
}

/// Opt-in, bounded localization diagnostics. Configure labels from trusted
/// deployment configuration, not from individual requests.
#[derive(Debug)]
pub struct LocalizationDiagnostics {
    counts: Mutex<LocalizationSnapshot>,
}

impl LocalizationDiagnostics {
    /// Create counters for at most [`MAX_DIAGNOSTIC_LOCALES`] distinct locales.
    ///
    /// # Errors
    /// Returns [`ErrorCode::InvalidRequest`] if the limit is exceeded.
    pub fn new(locales: impl IntoIterator<Item = Locale>) -> Result<Self, EngineError> {
        let mut counts = BTreeMap::new();
        for locale in locales {
            if !counts.contains_key(&locale) && counts.len() == MAX_DIAGNOSTIC_LOCALES {
                return Err(EngineError::new(
                    ErrorCode::InvalidRequest,
                    "too many diagnostic locales",
                ));
            }
            counts.insert(locale, 0);
        }
        Ok(Self {
            counts: Mutex::new(LocalizationSnapshot {
                locales: counts,
                other: 0,
            }),
        })
    }

    /// Return a consistent snapshot; counts saturate rather than wrapping.
    #[must_use]
    pub fn snapshot(&self) -> LocalizationSnapshot {
        self.counts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn record_missing(&self, locale: &Locale) {
        let mut counts = self
            .counts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let count = match counts.locales.get_mut(locale) {
            Some(count) => count,
            None => &mut counts.other,
        };
        *count = count.saturating_add(1);
    }
}

impl CatalogSet {
    /// Create an empty set.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a catalog.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::Conflict`] if the locale is already present.
    pub fn insert(&mut self, catalog: MessageCatalog) -> Result<(), EngineError> {
        if self.catalogs.contains_key(catalog.locale()) {
            return Err(
                EngineError::new(ErrorCode::Conflict, "duplicate locale catalog")
                    .with_detail("locale", catalog.locale().to_string()),
            );
        }
        self.catalogs.insert(catalog.locale().clone(), catalog);
        Ok(())
    }

    /// Look up a catalog by exact locale.
    #[must_use]
    pub fn catalog(&self, locale: &Locale) -> Option<&MessageCatalog> {
        self.catalogs.get(locale)
    }

    /// Iterate over catalogs in locale order.
    pub fn iter(&self) -> impl Iterator<Item = (&Locale, &MessageCatalog)> {
        self.catalogs.iter()
    }

    /// Resolve a key through the requested locale's fallback chain.
    ///
    /// The chain is exact tag, language, then `pt-BR`.
    #[must_use]
    pub fn resolve(&self, key: &MessageKey, requested: &Locale) -> Option<ResolvedMessage<'_>> {
        for candidate in requested.fallbacks() {
            if let Some((locale, catalog)) = self.catalogs.get_key_value(&candidate)
                && let Some(text) = catalog.get(key)
            {
                return Some(ResolvedMessage {
                    text,
                    locale,
                    fallback: locale != requested,
                });
            }
        }
        None
    }

    /// Resolve normally and count fallback or missing results by requested locale.
    /// Exact hits do not increment a counter. The caller must use this method
    /// for each lookup it wants included; plain `resolve` is not instrumented.
    #[must_use]
    pub fn resolve_with_diagnostics(
        &self,
        key: &MessageKey,
        requested: &Locale,
        diagnostics: &LocalizationDiagnostics,
    ) -> Option<ResolvedMessage<'_>> {
        let resolved = self.resolve(key, requested);
        if resolved.as_ref().is_none_or(|message| message.fallback) {
            diagnostics.record_missing(requested);
        }
        resolved
    }

    /// Keys present in `source` but missing from `locale`.
    ///
    /// Returns an empty vector when either catalog is absent.
    #[must_use]
    pub fn missing_keys(&self, locale: &Locale, source: &Locale) -> Vec<MessageKey> {
        let (Some(source), Some(target)) = (self.catalogs.get(source), self.catalogs.get(locale))
        else {
            return Vec::new();
        };
        source
            .messages
            .keys()
            .filter(|key| !target.messages.contains_key(*key))
            .cloned()
            .collect()
    }

    /// Number of loaded catalogs.
    #[must_use]
    pub fn len(&self) -> usize {
        self.catalogs.len()
    }

    /// Whether no catalogs are loaded.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.catalogs.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(value: &str) -> MessageKey {
        MessageKey::parse(value).unwrap()
    }

    fn catalog(locale: &str, messages: &[(&str, &str)]) -> MessageCatalog {
        let mut catalog = MessageCatalog::new(Locale::parse(locale).unwrap());
        for (key, text) in messages {
            catalog.insert(self::key(key), *text).unwrap();
        }
        catalog
    }

    #[test]
    fn brazilian_portuguese_is_the_default() {
        assert_eq!(DEFAULT_LOCALE, "pt-BR");
        assert_eq!(Locale::default().as_str(), "pt-BR");
    }

    #[test]
    fn locales_are_canonicalized() {
        assert_eq!(Locale::parse("pt-br").unwrap().as_str(), "pt-BR");
        assert_eq!(Locale::parse("EN_us").unwrap().as_str(), "en-US");
        assert_eq!(Locale::parse("zh-hant-tw").unwrap().as_str(), "zh-Hant-TW");
        assert_eq!(Locale::parse("es-419").unwrap().as_str(), "es-419");
    }

    #[test]
    fn malformed_locales_are_rejected() {
        for input in ["", "e", "pt-", "pt BR", "pt--BR", "123", "pt-BR-"] {
            assert!(
                Locale::parse(input).is_err(),
                "{input} should be rejected as a locale"
            );
        }
    }

    #[test]
    fn fallbacks_end_at_the_default_locale() {
        let pt = Locale::parse("pt-BR").unwrap();
        assert_eq!(
            pt.fallbacks()
                .iter()
                .map(Locale::as_str)
                .collect::<Vec<_>>(),
            ["pt-BR", "pt"]
        );

        let en = Locale::parse("en-US").unwrap();
        assert_eq!(
            en.fallbacks()
                .iter()
                .map(Locale::as_str)
                .collect::<Vec<_>>(),
            ["en-US", "en", "pt-BR"]
        );

        let portugal = Locale::parse("pt-PT").unwrap();
        assert_eq!(
            portugal
                .fallbacks()
                .iter()
                .map(Locale::as_str)
                .collect::<Vec<_>>(),
            ["pt-PT", "pt", "pt-BR"]
        );
    }

    #[test]
    fn message_keys_are_validated() {
        assert!(MessageKey::parse("entity.door.wooden.name").is_ok());
        assert!(MessageKey::parse("error.out_of_range").is_ok());
        assert!(MessageKey::parse("ui.inventory.capacity-2").is_ok());
        for input in [
            "",
            "Entity.Door",
            ".entity.door",
            "entity..door",
            "entity door",
            "entity.",
        ] {
            assert!(
                MessageKey::parse(input).is_err(),
                "{input} should be rejected as a key"
            );
        }
    }

    #[test]
    fn placeholders_are_extracted_and_compared() {
        assert_eq!(
            placeholders("{quantity} × {item} for {player_name}"),
            ["item", "player_name", "quantity"]
                .iter()
                .map(ToString::to_string)
                .collect()
        );
        assert!(placeholders("literal {not a placeholder}").is_empty());
        assert!(validate_placeholders("{count} itens", "{count} itens").is_ok());

        let error = validate_placeholders("{count} itens", "{total} itens").unwrap_err();
        assert_eq!(error.code(), ErrorCode::ContentInvalid);
        assert_eq!(error.details()["missing"], "count");
        assert_eq!(error.details()["extra"], "total");
    }

    #[test]
    fn catalogs_reject_duplicates_and_empty_messages() {
        let mut messages = catalog("pt-BR", &[("ui.title", "Título")]);
        assert_eq!(
            messages
                .insert(key("ui.title"), "Outro")
                .unwrap_err()
                .code(),
            ErrorCode::Conflict
        );
        assert_eq!(
            messages.insert(key("ui.empty"), "   ").unwrap_err().code(),
            ErrorCode::ContentInvalid
        );
        assert_eq!(messages.len(), 1);
    }

    #[test]
    fn resolution_prefers_exact_then_language_then_default() {
        let mut set = CatalogSet::new();
        set.insert(catalog(
            "pt-BR",
            &[("ui.title", "Título"), ("ui.only_pt", "Só aqui")],
        ))
        .unwrap();
        set.insert(catalog("en", &[("ui.title", "Title")])).unwrap();

        let en_us = Locale::parse("en-US").unwrap();
        let resolved = set.resolve(&key("ui.title"), &en_us).unwrap();
        assert_eq!(resolved.text, "Title");
        assert_eq!(resolved.locale.as_str(), "en");
        assert!(resolved.fallback);

        let resolved = set.resolve(&key("ui.only_pt"), &en_us).unwrap();
        assert_eq!(resolved.text, "Só aqui");
        assert_eq!(resolved.locale.as_str(), "pt-BR");
        assert!(resolved.fallback);

        assert!(set.resolve(&key("ui.missing"), &en_us).is_none());
    }

    #[test]
    fn diagnostics_count_fallbacks_and_missing_without_keys_or_unbounded_labels() {
        let mut set = CatalogSet::new();
        set.insert(catalog("en", &[("ui.title", "Title")])).unwrap();
        set.insert(catalog("pt-BR", &[("ui.title", "Título")]))
            .unwrap();
        let en = Locale::parse("en").unwrap();
        let en_us = Locale::parse("EN_us").unwrap();
        let diagnostics = LocalizationDiagnostics::new([en.clone(), en_us.clone()]).unwrap();
        assert!(
            !set.resolve_with_diagnostics(&key("ui.title"), &en, &diagnostics)
                .unwrap()
                .fallback
        );
        assert!(
            set.resolve_with_diagnostics(&key("ui.title"), &en_us, &diagnostics)
                .unwrap()
                .fallback
        );
        assert!(
            set.resolve_with_diagnostics(&key("ui.missing"), &en_us, &diagnostics)
                .is_none()
        );
        let other = Locale::parse("fr-FR").unwrap();
        assert!(
            set.resolve_with_diagnostics(&key("ui.title"), &other, &diagnostics)
                .unwrap()
                .fallback
        );
        let snapshot = diagnostics.snapshot();
        assert_eq!(snapshot.locales[&en], 0);
        assert_eq!(snapshot.locales[&en_us], 2);
        assert_eq!(snapshot.other, 1);
        assert_eq!(snapshot.locales.len(), 2);
        let locales =
            (0..=MAX_DIAGNOSTIC_LOCALES).map(|i| Locale::parse(&format!("en-{i}")).unwrap());
        assert!(LocalizationDiagnostics::new(locales).is_err());
    }

    #[test]
    fn missing_keys_compare_against_the_source_catalog() {
        let mut set = CatalogSet::new();
        set.insert(catalog(
            "pt-BR",
            &[("ui.title", "Título"), ("ui.subtitle", "Subtítulo")],
        ))
        .unwrap();
        set.insert(catalog("en", &[("ui.title", "Title")])).unwrap();

        let missing = set.missing_keys(
            &Locale::parse("en").unwrap(),
            &Locale::parse("pt-BR").unwrap(),
        );
        assert_eq!(missing, [key("ui.subtitle")]);
    }

    #[test]
    fn locale_and_catalog_round_trip_through_json() {
        let locale = Locale::parse("pt-br").unwrap();
        let json = serde_json::to_string(&locale).unwrap();
        assert_eq!(json, "\"pt-BR\"");
        assert_eq!(serde_json::from_str::<Locale>(&json).unwrap(), locale);

        let messages = catalog("pt-BR", &[("ui.title", "Título")]);
        let json = serde_json::to_string(&messages).unwrap();
        let parsed: MessageCatalog = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, messages);
    }
}
