//! Hash verification and native loading of one grammar plugin.
//!
//! Order matters and is the security model: (1) check the manifest's ABI claim,
//! (2) open every file with write/delete sharing denied and hash it against the
//! manifest, (3) only then `LoadLibrary` the DLL, (4) check the ABI the library itself
//! reports, (5) compile the queries. A DLL that fails (1) or (2) is never mapped.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::Read;
use std::path::Path;

use libloading::Library;
use sha2::{Digest, Sha256};
use tree_sitter::{Language, LANGUAGE_VERSION, MIN_COMPATIBLE_LANGUAGE_VERSION};
use tree_sitter_highlight::HighlightConfiguration;
use tree_sitter_language::LanguageFn;

use super::error::GrammarError;
use super::index::hex;
use super::manifest::{
    Manifest, DLL_FILE, HIGHLIGHTS_FILE, INJECTIONS_FILE, LOCALS_FILE, MAX_FILE_BYTES,
    MAX_QUERY_BYTES,
};

/// A grammar loaded into the process. Always leaked by the registry: the DLL must stay
/// mapped for as long as any `Language` / query created from it is alive, which is the
/// whole process lifetime.
pub(crate) struct Loaded {
    // Field order is irrelevant (never dropped) but kept: users before the library.
    pub language: Language,
    pub config: HighlightConfiguration,
    pub _lib: Library,
}

/// Files that passed verification: handles kept open (so nobody can rewrite them
/// before the DLL is mapped) and the decoded query texts.
pub(crate) struct Verified {
    pub _handles: Vec<File>,
    pub texts: HashMap<String, String>,
}

/// Open read-only, denying other writers/deleters on Windows.
fn open_locked(path: &Path) -> std::io::Result<File> {
    let mut o = OpenOptions::new();
    o.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // FILE_SHARE_READ only.
        o.share_mode(1);
    }
    o.open(path)
}

/// Check the ABI number a manifest claims, before touching any native code.
pub(crate) fn check_abi(found: usize) -> Result<(), GrammarError> {
    if (MIN_COMPATIBLE_LANGUAGE_VERSION..=LANGUAGE_VERSION).contains(&found) {
        Ok(())
    } else {
        Err(GrammarError::AbiIncompatible {
            found,
            min: MIN_COMPATIBLE_LANGUAGE_VERSION,
            max: LANGUAGE_VERSION,
        })
    }
}

/// Hash every file listed in `manifest` inside `dir`.
pub(crate) fn verify_files(dir: &Path, manifest: &Manifest) -> Result<Verified, GrammarError> {
    let mut handles = Vec::with_capacity(manifest.files.len());
    let mut texts = HashMap::new();
    for (name, expected) in &manifest.files {
        let path = dir.join(name);
        let mut file = match open_locked(&path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(GrammarError::MissingFile { file: name.clone() })
            }
            Err(e) => return Err(GrammarError::io(&path, &e)),
        };
        let meta = file.metadata().map_err(|e| GrammarError::io(&path, &e))?;
        if !meta.is_file() {
            return Err(GrammarError::manifest(&path, "not a regular file"));
        }
        let is_query = name.ends_with(".scm");
        let limit = if is_query {
            MAX_QUERY_BYTES
        } else {
            MAX_FILE_BYTES
        };
        if meta.len() > limit {
            return Err(GrammarError::manifest(&path, "file is too large"));
        }
        let mut hasher = Sha256::new();
        let mut text = Vec::new();
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let n = file
                .read(&mut buf)
                .map_err(|e| GrammarError::io(&path, &e))?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
            if is_query {
                text.extend_from_slice(&buf[..n]);
            }
        }
        let actual = hex(&hasher.finalize());
        if actual != *expected {
            return Err(GrammarError::HashMismatch {
                file: name.clone(),
                expected: expected.clone(),
                actual,
            });
        }
        if is_query {
            let s = String::from_utf8(text)
                .map_err(|_| GrammarError::manifest(&path, "query is not valid UTF-8"))?;
            texts.insert(name.clone(), s);
        }
        handles.push(file);
    }
    Ok(Verified {
        _handles: handles,
        texts,
    })
}

#[cfg(windows)]
unsafe fn open_library(path: &Path) -> Result<Library, libloading::Error> {
    use libloading::os::windows::{
        Library as WinLibrary, LOAD_LIBRARY_SEARCH_DEFAULT_DIRS, LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR,
    };
    // Never the process CWD or PATH: only the application, System32 and the DLL's folder.
    // SAFETY: forwarded; the caller verified the file's sha256.
    unsafe {
        WinLibrary::load_with_flags(
            path,
            LOAD_LIBRARY_SEARCH_DEFAULT_DIRS | LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR,
        )
    }
    .map(Library::from)
}

#[cfg(not(windows))]
unsafe fn open_library(path: &Path) -> Result<Library, libloading::Error> {
    // SAFETY: forwarded; the caller verified the file's sha256.
    unsafe { Library::new(path) }
}

/// Verify and load the plugin in `dir`.
pub(crate) fn load_plugin(dir: &Path, manifest: &Manifest) -> Result<Loaded, GrammarError> {
    check_abi(manifest.tree_sitter_abi as usize)?;
    let verified = verify_files(dir, manifest)?;

    let dll = std::path::absolute(dir.join(DLL_FILE)).map_err(|e| GrammarError::io(dir, &e))?;
    // SAFETY: the DLL's sha256 matched the manifest a moment ago and the verified handle
    // (write/delete denied) is held until this function returns. Loading runs its
    // initialisers: that is the trust we place in the hashed, index-published binary.
    let lib = unsafe { open_library(&dll) }.map_err(|e| GrammarError::Load {
        path: dll.display().to_string(),
        message: e.to_string(),
    })?;

    // SAFETY: the symbol is by contract `extern "C" fn() -> *const ()` (tree-sitter's
    // grammar entry point).
    let raw: unsafe extern "C" fn() -> *const () =
        match unsafe { lib.get::<unsafe extern "C" fn() -> *const ()>(manifest.symbol.as_bytes()) }
        {
            Ok(sym) => *sym,
            Err(e) => {
                return Err(GrammarError::MissingSymbol {
                    symbol: manifest.symbol.clone(),
                    message: e.to_string(),
                })
            }
        };
    // SAFETY: calling the grammar entry point; it only returns a pointer to static data.
    if unsafe { raw() }.is_null() {
        return Err(GrammarError::BadLanguage("null language pointer".into()));
    }
    // SAFETY: `raw` is a valid grammar entry point (checked above).
    let language: Language = unsafe { LanguageFn::from_raw(raw) }.into();

    let abi = language.abi_version();
    check_abi(abi)?;
    if abi != manifest.tree_sitter_abi as usize {
        return Err(GrammarError::BadLanguage(format!(
            "manifest declares ABI {} but the library reports {abi}",
            manifest.tree_sitter_abi
        )));
    }

    let text = |name: &str| verified.texts.get(name).map_or("", String::as_str);
    let config = crate::highlight::make_config(
        language.clone(),
        &manifest.name,
        text(HIGHLIGHTS_FILE),
        text(INJECTIONS_FILE),
        text(LOCALS_FILE),
    )
    .map_err(GrammarError::Query)?;
    drop(verified);
    Ok(Loaded {
        language,
        config,
        _lib: lib,
    })
}
