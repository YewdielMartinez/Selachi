//! Fonts embedded into the binary (see `assets/fonts/README.md`).

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, PoisonError, RwLock};

pub const UI_FAMILY: &str = "DM Sans";
pub const MONO_FAMILY: &str = "Geist Mono";

/// Families installed in the system plus the embedded ones: lower-case name to its real spelling.
/// `None` until the app registers them (tests and examples then trust any name).
static INSTALLED: RwLock<Option<HashMap<String, String>>> = RwLock::new(None);

/// Where the installed families come from, asked the first time a non-embedded family is needed
/// (listing the system fonts costs a few milliseconds the default fonts never pay).
type Source = Box<dyn Fn() -> Vec<String> + Send + Sync>;
static SOURCE: RwLock<Option<Source>> = RwLock::new(None);

fn index(names: impl IntoIterator<Item = String>) -> HashMap<String, String> {
    names.into_iter().map(|n| (n.to_lowercase(), n)).collect()
}

/// Records the font families the text system can resolve (`TextSystem::all_font_names`), so a
/// configured name that is not installed falls back to the default (see [`resolve`]).
pub fn register_installed<I: IntoIterator<Item = String>>(names: I) {
    *INSTALLED.write().unwrap_or_else(PoisonError::into_inner) = Some(index(names));
}

/// Like [`register_installed`], but `source` is only called when a family other than the embedded
/// ones is looked up.
pub fn register_source(source: impl Fn() -> Vec<String> + Send + Sync + 'static) {
    *INSTALLED.write().unwrap_or_else(PoisonError::into_inner) = None;
    *SOURCE.write().unwrap_or_else(PoisonError::into_inner) = Some(Box::new(source));
}

/// Forgets the registered families (every name is trusted again).
pub fn clear_installed() {
    *INSTALLED.write().unwrap_or_else(PoisonError::into_inner) = None;
    *SOURCE.write().unwrap_or_else(PoisonError::into_inner) = None;
}

/// `Some(spelling)` of the installed family called `name`; `Some(None)`: it is not installed;
/// `None`: the installed families are unknown (nothing registered).
fn lookup(name: &str) -> Option<Option<String>> {
    let key = name.to_lowercase();
    if let Some(map) = INSTALLED
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .as_ref()
    {
        return Some(map.get(&key).cloned());
    }
    let names = SOURCE
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .as_ref()
        .map(|source| source())?;
    let map = index(names);
    let found = map.get(&key).cloned();
    *INSTALLED.write().unwrap_or_else(PoisonError::into_inner) = Some(map);
    Some(found)
}

fn embedded_family(name: &str) -> Option<&'static str> {
    [UI_FAMILY, MONO_FAMILY]
        .into_iter()
        .find(|f| f.eq_ignore_ascii_case(name))
}

/// Can `family` be used? The embedded families always can; the others only when the app registered
/// the installed ones (before that, every non-empty name is trusted).
pub fn is_available(family: &str) -> bool {
    let name = family.trim();
    if name.is_empty() {
        return false;
    }
    embedded_family(name).is_some() || lookup(name).is_none_or(|found| found.is_some())
}

/// Leaks `s` once per distinct value: typography keeps `&'static str` families so that it stays
/// `Copy`. Only available families are interned, so the set is bounded by the installed fonts.
fn intern(s: &str) -> &'static str {
    static SET: Mutex<Option<HashSet<&'static str>>> = Mutex::new(None);
    let mut guard = SET.lock().unwrap_or_else(PoisonError::into_inner);
    let set = guard.get_or_insert_with(HashSet::new);
    if let Some(found) = set.get(s) {
        return found;
    }
    let leaked: &'static str = Box::leak(s.to_string().into_boxed_str());
    set.insert(leaked);
    leaked
}

/// The family to use for the configured `requested` name: the name as the system spells it when it
/// is available, otherwise `default`.
pub fn resolve(requested: &str, default: &'static str) -> &'static str {
    let name = requested.trim();
    if name.is_empty() {
        return default;
    }
    if let Some(embedded) = embedded_family(name) {
        return embedded;
    }
    match lookup(name) {
        Some(None) => default,
        Some(Some(spelling)) => intern(&spelling),
        None => intern(name),
    }
}

/// Raw TTF data for every embedded face, ready for `TextSystem::add_fonts`.
pub fn embedded() -> Vec<Cow<'static, [u8]>> {
    vec![
        Cow::Borrowed(&include_bytes!("../../../assets/fonts/DMSans-Regular.ttf")[..]),
        Cow::Borrowed(&include_bytes!("../../../assets/fonts/DMSans-Medium.ttf")[..]),
        Cow::Borrowed(&include_bytes!("../../../assets/fonts/DMSans-SemiBold.ttf")[..]),
        Cow::Borrowed(&include_bytes!("../../../assets/fonts/GeistMono-Regular.ttf")[..]),
        Cow::Borrowed(&include_bytes!("../../../assets/fonts/GeistMono-Medium.ttf")[..]),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_families_fall_back_and_known_ones_keep_their_spelling() {
        // One test mutates the global registry, so it checks everything in order.
        clear_installed();
        assert_eq!(
            resolve("Whatever", UI_FAMILY),
            "Whatever",
            "unregistered: trusted"
        );
        register_installed(["Cascadia Mono".to_string(), "Segoe UI".to_string()]);
        assert_eq!(resolve("cascadia mono", MONO_FAMILY), "Cascadia Mono");
        assert_eq!(resolve("Nope Sans", UI_FAMILY), UI_FAMILY);
        assert_eq!(resolve("  ", MONO_FAMILY), MONO_FAMILY);
        assert_eq!(
            resolve("geist mono", UI_FAMILY),
            MONO_FAMILY,
            "embedded always"
        );
        assert!(!is_available("Nope Sans") && is_available("Segoe UI"));
        // the same name interns to the same pointer
        assert!(std::ptr::eq(
            resolve("segoe ui", UI_FAMILY),
            resolve("Segoe UI", UI_FAMILY)
        ));
        // A source is only asked when a non-embedded family is looked up, and only once.
        let asked = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = asked.clone();
        register_source(move || {
            counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            vec!["Consolas".to_string()]
        });
        assert_eq!(resolve("dm sans", MONO_FAMILY), UI_FAMILY);
        assert_eq!(asked.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert_eq!(resolve("consolas", UI_FAMILY), "Consolas");
        assert_eq!(resolve("Missing", UI_FAMILY), UI_FAMILY);
        assert_eq!(asked.load(std::sync::atomic::Ordering::SeqCst), 1);
        clear_installed();
    }

    #[test]
    fn fonts_are_truetype() {
        let all = embedded();
        assert_eq!(all.len(), 5);
        for f in all {
            assert!(f.len() > 10_000);
            assert_eq!(&f[..4], &[0x00, 0x01, 0x00, 0x00]);
        }
    }
}
