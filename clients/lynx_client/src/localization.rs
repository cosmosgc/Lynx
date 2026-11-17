//! CLT-010 client locale catalogs, message resolution, and persisted choice.
//!
//! Player-facing widget text is addressed by stable message keys. The embedded
//! `pt-BR` source catalog and the optional `en` translation are shared with the
//! content pipeline, so a missing key renders the key itself instead of
//! panicking or blocking a frame.

use std::{
    collections::BTreeSet,
    env,
    fs::{self, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
    time::{SystemTime, UNIX_EPOCH},
};

use bevy::prelude::Resource;
use engine_core::i18n::{CatalogSet, Locale, MessageCatalog, MessageKey};
use serde::{Deserialize, Serialize};

/// Environment override for the persisted locale document.
pub const LOCALE_PATH_ENV: &str = "LYNX_LOCALE_PATH";

/// Locales shipped with this client build, in selector order.
pub const SUPPORTED_LOCALES: [&str; 2] = ["pt-BR", "en"];

const MAX_LOCALE_FILE_BYTES: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq, Resource, Default)]
pub struct LocalePreference {
    locale: Locale,
    path: Option<PathBuf>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct LocaleDocument {
    locale: Locale,
}

impl LocalePreference {
    /// Load the persisted choice, falling back to `pt-BR` when the document is
    /// absent, unreadable, oversized, or malformed.
    #[must_use]
    pub fn load(path: Option<PathBuf>) -> Self {
        let locale = path.as_deref().and_then(read_locale).unwrap_or_default();
        Self { locale, path }
    }

    /// Load from the platform configuration directory or the environment
    /// override.
    #[must_use]
    pub fn load_default() -> Self {
        Self::load(default_path())
    }

    /// The effective locale.
    #[must_use]
    pub fn locale(&self) -> &Locale {
        &self.locale
    }

    /// Persisted document location, when one is configured.
    #[must_use]
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Select a locale and persist it when a path is configured.
    ///
    /// The in-memory selection changes even if the write fails, so a read-only
    /// configuration directory does not lock the player out of the selector.
    ///
    /// # Errors
    ///
    /// Returns the write error so the caller can surface or log it.
    pub fn set_locale(&mut self, locale: Locale) -> io::Result<()> {
        if self.locale == locale {
            return Ok(());
        }
        self.locale = locale;
        match &self.path {
            Some(path) => write_locale(path, &self.locale),
            None => Ok(()),
        }
    }
}

/// Default locale document location: `LYNX_LOCALE_PATH`, else the platform
/// configuration directory beside the server profile document.
#[must_use]
pub fn default_path() -> Option<PathBuf> {
    env::var_os(LOCALE_PATH_ENV)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            crate::server_browser::platform_config_dir()
                .map(|directory| directory.join("lynx").join("locale.json"))
        })
}

fn read_locale(path: &Path) -> Option<Locale> {
    let mut bytes = Vec::new();
    fs::File::open(path)
        .ok()?
        .take(u64::try_from(MAX_LOCALE_FILE_BYTES).ok()? + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() > MAX_LOCALE_FILE_BYTES {
        return None;
    }
    let document: LocaleDocument = serde_json::from_slice(&bytes).ok()?;
    SUPPORTED_LOCALES
        .iter()
        .any(|supported| document.locale.as_str() == *supported)
        .then_some(document.locale)
}

fn write_locale(path: &Path, locale: &Locale) -> io::Result<()> {
    let json = serde_json::to_vec(&LocaleDocument {
        locale: locale.clone(),
    })
    .map_err(|_| io::Error::other("could not encode the locale document"))?;
    if json.len() > MAX_LOCALE_FILE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "locale document exceeds the size limit",
        ));
    }
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)?;
    }
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let mut temporary = path.as_os_str().to_owned();
    temporary.push(format!(".{}.{}.tmp", std::process::id(), stamp));
    let temporary = PathBuf::from(temporary);
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(&json)?;
        file.sync_all()?;
        fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

/// Embedded catalogs shared by every widget and error path.
#[must_use]
pub fn catalogs() -> Option<&'static CatalogSet> {
    static CATALOGS: OnceLock<Option<CatalogSet>> = OnceLock::new();
    CATALOGS
        .get_or_init(|| {
            let pt_br = ron::from_str::<MessageCatalog>(include_str!(
                "../../../content/localization/pt-BR.ron"
            ))
            .ok()?;
            let en = ron::from_str::<MessageCatalog>(include_str!(
                "../../../content/localization/en.ron"
            ))
            .ok()?;
            let mut catalogs = CatalogSet::new();
            catalogs.insert(pt_br).ok()?;
            catalogs.insert(en).ok()?;
            Some(catalogs)
        })
        .as_ref()
}

/// Resolve `key` in `locale` through the fallback chain.
///
/// A missing or malformed key renders the key text itself and is reported once
/// per locale and key in development builds.
#[must_use]
pub fn text(locale: &Locale, key: &str) -> String {
    text_with(locale, key, &[])
}

/// Resolve `key` and substitute `{name}` placeholders in one pass.
///
/// Missing placeholders keep their braces; callers never concatenate translated
/// fragments to build a sentence.
#[must_use]
pub fn text_with(locale: &Locale, key: &str, args: &[(&str, &str)]) -> String {
    let Ok(message_key) = MessageKey::parse(key) else {
        return key.to_string();
    };
    let Some(template) = catalogs()
        .and_then(|catalogs| catalogs.resolve(&message_key, locale))
        .map(|message| message.text)
    else {
        note_missing(locale, key);
        return key.to_string();
    };
    let mut rendered = template.to_string();
    for (name, value) in args {
        rendered = rendered.replace(&format!("{{{name}}}"), value);
    }
    rendered
}

/// Render a stable player error code, mapping the two protocol aliases that
/// predate the catalog.
#[must_use]
pub fn error_text(locale: &Locale, code: Option<&str>) -> String {
    let code = match code {
        Some("forbidden") => "permission_denied",
        Some("unauthorized") => "session_expired",
        Some(code) => code,
        None => "internal",
    };
    text(locale, &format!("error.{code}"))
}

fn note_missing(locale: &Locale, key: &str) {
    static REPORTED: OnceLock<Mutex<BTreeSet<String>>> = OnceLock::new();
    let reported = REPORTED.get_or_init(|| Mutex::new(BTreeSet::new()));
    let label = format!("{locale}:{key}");
    let first = reported
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(label);
    if first && cfg!(debug_assertions) {
        eprintln!("missing localization key `{key}` for locale `{locale}`");
    }
}

#[cfg_attr(coverage_nightly, coverage(off))]
#[cfg(test)]
mod tests {
    use super::*;

    fn locale(tag: &str) -> Locale {
        Locale::parse(tag).unwrap()
    }

    #[test]
    fn brazilian_portuguese_is_the_default_and_source() {
        assert_eq!(LocalePreference::default().locale().as_str(), "pt-BR");
        let pt_br = catalogs().unwrap().catalog(&locale("pt-BR")).unwrap();
        assert_eq!(
            pt_br.get(&MessageKey::parse("ui.inventory.title").unwrap()),
            Some("Inventário")
        );
    }

    #[test]
    fn english_resolves_widget_text_and_placeholders() {
        let en = locale("en");
        assert_eq!(text(&en, "ui.inventory.equip"), "Equip");
        assert_eq!(
            text_with(
                &en,
                "ui.inventory.row",
                &[("slot", "1"), ("item", "Sword"), ("quantity", "3")]
            ),
            "Slot 1: Sword x3"
        );
    }

    #[test]
    fn unknown_locale_falls_back_to_the_source_catalog() {
        let french = locale("fr-FR");
        assert_eq!(text(&french, "ui.inventory.title"), "Inventário");
    }

    #[test]
    fn missing_or_malformed_keys_render_the_key_and_do_not_panic() {
        assert_eq!(text(&Locale::default(), "ui.missing"), "ui.missing");
        assert_eq!(text(&Locale::default(), "Not A Key"), "Not A Key");
        assert_eq!(
            text(&Locale::default(), "ui.inventory.row"),
            "Slot {slot}: {item} x{quantity}"
        );
    }

    #[test]
    fn every_english_key_exists_in_the_source_catalog() {
        let missing = catalogs()
            .unwrap()
            .missing_keys(&locale("en"), &Locale::default());
        assert!(missing.is_empty(), "missing English keys: {missing:?}");
    }

    #[test]
    fn every_supported_locale_has_a_catalog() {
        for tag in SUPPORTED_LOCALES {
            assert!(
                catalogs().unwrap().catalog(&locale(tag)).is_some(),
                "{tag} catalog is embedded"
            );
        }
    }

    #[test]
    fn error_aliases_and_unknown_codes_resolve_through_the_catalog() {
        let en = locale("en");
        assert_eq!(
            error_text(&en, Some("forbidden")),
            "You do not have permission to perform this action."
        );
        assert_eq!(
            error_text(&en, Some("unauthorized")),
            "Your session expired. Sign in again."
        );
        assert_eq!(
            error_text(&en, None),
            "The action could not be completed. Try again later."
        );
    }

    #[test]
    fn locale_document_round_trips_and_ignores_unsupported_tags() {
        let directory = std::env::temp_dir().join(format!("lynx-locale-{}", std::process::id()));
        let path = directory.join("locale.json");
        let mut preference = LocalePreference::load(Some(path.clone()));
        assert_eq!(preference.locale().as_str(), "pt-BR");
        preference.set_locale(locale("en")).unwrap();
        assert_eq!(
            LocalePreference::load(Some(path.clone())).locale().as_str(),
            "en"
        );

        fs::write(&path, br#"{"locale":"de-DE"}"#).unwrap();
        assert_eq!(
            LocalePreference::load(Some(path.clone())).locale().as_str(),
            "pt-BR",
            "unsupported locales fall back instead of failing"
        );
        fs::write(&path, b"not json").unwrap();
        assert_eq!(
            LocalePreference::load(Some(path.clone())).locale().as_str(),
            "pt-BR"
        );
        fs::write(&path, [b'x'; MAX_LOCALE_FILE_BYTES + 1]).unwrap();
        assert_eq!(
            LocalePreference::load(Some(path.clone())).locale().as_str(),
            "pt-BR"
        );
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn in_memory_selection_works_without_a_persisted_path() {
        let mut preference = LocalePreference::default();
        preference.set_locale(locale("en")).unwrap();
        assert_eq!(preference.locale().as_str(), "en");
        assert!(preference.path().is_none());
    }
}
