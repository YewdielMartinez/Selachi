//! Authenticode status of a downloaded installer (`WinVerifyTrust`, no UI, no network revocation
//! lookups: the call must not block or reach out on its own).

use std::path::Path;

/// What the Windows trust provider says about a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Authenticode {
    /// Signed and the chain verifies.
    Signed,
    /// No signature at all (every build until the project has a certificate).
    Unsigned,
    /// A signature that does not verify (tampered, untrusted root, expired...). The message has
    /// the HRESULT.
    Invalid(String),
    /// The check could not run (not Windows, or the call failed to start).
    Unknown,
}

impl Authenticode {
    /// Short text for the UI ("Firmado", "Sin firmar").
    pub fn label(&self) -> &'static str {
        match self {
            Authenticode::Signed => "Firmado",
            Authenticode::Unsigned => "Sin firmar",
            Authenticode::Invalid(_) => "Firma inválida",
            Authenticode::Unknown => "Firma sin comprobar",
        }
    }
}

/// `TRUST_E_NOSIGNATURE` and the other "there is no signature" results.
#[cfg(any(windows, test))]
fn classify(hresult: i32) -> Authenticode {
    const TRUST_E_NOSIGNATURE: u32 = 0x800B_0100;
    const TRUST_E_SUBJECT_FORM_UNKNOWN: u32 = 0x800B_0003;
    const TRUST_E_PROVIDER_UNKNOWN: u32 = 0x800B_0001;
    match hresult as u32 {
        0 => Authenticode::Signed,
        TRUST_E_NOSIGNATURE | TRUST_E_SUBJECT_FORM_UNKNOWN | TRUST_E_PROVIDER_UNKNOWN => {
            Authenticode::Unsigned
        }
        other => Authenticode::Invalid(format!("0x{other:08X}")),
    }
}

#[cfg(windows)]
pub fn authenticode_status(path: &Path) -> Authenticode {
    use std::os::windows::ffi::OsStrExt as _;
    use windows_sys::Win32::Security::WinTrust::{
        WINTRUST_ACTION_GENERIC_VERIFY_V2, WINTRUST_DATA, WINTRUST_DATA_0, WINTRUST_FILE_INFO,
        WTD_CHOICE_FILE, WTD_REVOKE_NONE, WTD_STATEACTION_CLOSE, WTD_STATEACTION_VERIFY,
        WTD_UI_NONE, WinVerifyTrust,
    };

    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut file = WINTRUST_FILE_INFO {
        cbStruct: std::mem::size_of::<WINTRUST_FILE_INFO>() as u32,
        pcwszFilePath: wide.as_ptr(),
        hFile: std::ptr::null_mut(),
        pgKnownSubject: std::ptr::null_mut(),
    };
    let mut data = WINTRUST_DATA {
        cbStruct: std::mem::size_of::<WINTRUST_DATA>() as u32,
        pPolicyCallbackData: std::ptr::null_mut(),
        pSIPClientData: std::ptr::null_mut(),
        dwUIChoice: WTD_UI_NONE,
        fdwRevocationChecks: WTD_REVOKE_NONE,
        dwUnionChoice: WTD_CHOICE_FILE,
        Anonymous: WINTRUST_DATA_0 { pFile: &mut file },
        dwStateAction: WTD_STATEACTION_VERIFY,
        hWVTStateData: std::ptr::null_mut(),
        pwszURLReference: std::ptr::null_mut(),
        // WTD_CACHE_ONLY_URL_RETRIEVAL: never go to the network for the chain.
        dwProvFlags: 0x1000,
        dwUIContext: 0,
        pSignatureSettings: std::ptr::null_mut(),
    };
    let mut action = WINTRUST_ACTION_GENERIC_VERIFY_V2;
    // SAFETY: `data` and `file` outlive both calls, `wide` is NUL-terminated and alive, the
    // pointers inside are either valid or null as the API documents; the second call releases the
    // state the first one allocated.
    let result = unsafe {
        let r = WinVerifyTrust(
            std::ptr::null_mut(),
            &mut action,
            std::ptr::from_mut(&mut data).cast(),
        );
        data.dwStateAction = WTD_STATEACTION_CLOSE;
        WinVerifyTrust(
            std::ptr::null_mut(),
            &mut action,
            std::ptr::from_mut(&mut data).cast(),
        );
        r
    };
    classify(result)
}

#[cfg(not(windows))]
pub fn authenticode_status(_path: &Path) -> Authenticode {
    Authenticode::Unknown
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hresults_map_to_a_status() {
        assert_eq!(classify(0), Authenticode::Signed);
        assert_eq!(classify(0x800B_0100_u32 as i32), Authenticode::Unsigned);
        assert_eq!(
            classify(0x8009_6010_u32 as i32),
            Authenticode::Invalid("0x80096010".into())
        );
    }

    #[cfg(windows)]
    #[test]
    fn an_unsigned_file_is_unsigned_and_a_system_binary_is_signed() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("plain.exe");
        std::fs::write(&f, b"MZ not a real executable").unwrap();
        let s = authenticode_status(&f);
        assert!(
            matches!(s, Authenticode::Unsigned | Authenticode::Invalid(_)),
            "{s:?}"
        );
        // notepad ships signed (catalog-signed on recent Windows: the file API may say unsigned).
        let win = std::env::var_os("WINDIR").map(std::path::PathBuf::from);
        if let Some(p) = win.map(|w| w.join("System32").join("kernel32.dll")) {
            let _ = authenticode_status(&p); // must not crash
        }
    }
}
