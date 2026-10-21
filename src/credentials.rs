//! API tokens belong to the Windows user, never to repository configuration.
use windows_sys::Win32::Security::Credentials::*;

fn target(endpoint: &str) -> Vec<u16> {
    format!("git-rustle:api:{endpoint}\0")
        .encode_utf16()
        .collect()
}

pub fn read(endpoint: &str) -> Result<Option<String>, String> {
    let name = target(endpoint);
    let mut ptr = std::ptr::null_mut();
    // SAFETY: NUL-terminated UTF-16 name and valid output pointer; CredFree owns
    // the entire returned allocation, which is copied before being released.
    unsafe {
        if CredReadW(name.as_ptr(), CRED_TYPE_GENERIC, 0, &mut ptr) == 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(1168) {
                return Ok(None);
            }
            return Err(format!("Cannot read Windows Credential Manager: {error}"));
        }
        let credential = &*ptr;
        let result = if credential.CredentialBlobSize == 0 {
            Ok(None)
        } else {
            String::from_utf8(
                std::slice::from_raw_parts(
                    credential.CredentialBlob,
                    credential.CredentialBlobSize as usize,
                )
                .to_vec(),
            )
            .map(Some)
            .map_err(|_| "Saved API token has invalid encoding".to_string())
        };
        CredFree(ptr.cast());
        result
    }
}

pub fn write(endpoint: &str, token: &str) -> Result<(), String> {
    if token.is_empty() || token.len() > 2560 || token.chars().any(char::is_control) {
        return Err("Token must contain 1–2560 bytes without control characters".into());
    }
    let mut name = target(endpoint);
    let mut username: Vec<u16> = "git-rustle\0".encode_utf16().collect();
    let mut blob = token.as_bytes().to_vec();
    let credential = CREDENTIALW {
        Type: CRED_TYPE_GENERIC,
        TargetName: name.as_mut_ptr(),
        CredentialBlobSize: blob.len() as u32,
        CredentialBlob: blob.as_mut_ptr(),
        Persist: CRED_PERSIST_LOCAL_MACHINE,
        UserName: username.as_mut_ptr(),
        ..Default::default()
    };
    // SAFETY: all pointers above stay valid for the call; Windows copies the blob.
    let ok = unsafe { CredWriteW(&credential, 0) };
    if ok == 0 {
        Err(format!(
            "Cannot save token in Windows Credential Manager: {}",
            std::io::Error::last_os_error()
        ))
    } else {
        Ok(())
    }
}

pub fn delete(endpoint: &str) -> Result<(), String> {
    let name = target(endpoint);
    // SAFETY: NUL-terminated UTF-16 target, scoped to this application's API token.
    if unsafe { CredDeleteW(name.as_ptr(), CRED_TYPE_GENERIC, 0) } == 0 {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() != Some(1168) {
            return Err(format!("Cannot remove saved token: {error}"));
        }
    }
    Ok(())
}

/// Compatibility for tokens formerly saved against a single project. Only a
/// single unambiguous account within this exact API installation may be shared.
pub fn legacy_shared(base: &str, project_segment: &str) -> Result<Option<String>, String> {
    if base.contains('*') {
        return Ok(None);
    }
    let prefix = format!("git-rustle:api:{base}/{project_segment}/");
    let filter: Vec<u16> = format!("{prefix}*\0").encode_utf16().collect();
    let mut count = 0;
    let mut credentials = std::ptr::null_mut();
    // SAFETY: Windows owns the returned pointer array and entries until CredFree;
    // every blob is copied before release. The prefix restricts enumeration.
    unsafe {
        if CredEnumerateW(filter.as_ptr(), 0, &mut count, &mut credentials) == 0 {
            let error = std::io::Error::last_os_error();
            // Enumeration is optional compatibility lookup. Some restricted
            // Windows logon sessions cannot enumerate even when an absent-key
            // CredRead succeeds; retain normal settings/environment auth there.
            if optional_legacy_absence(error.raw_os_error()) {
                return Ok(None);
            }
            return Err(format!("Cannot read legacy API credentials: {error}"));
        }
        let result = (|| {
            let mut tokens = Vec::new();
            for ptr in std::slice::from_raw_parts(credentials, count as usize) {
                let credential = &**ptr;
                if credential.CredentialBlobSize == 0 {
                    continue;
                }
                let bytes = std::slice::from_raw_parts(
                    credential.CredentialBlob,
                    credential.CredentialBlobSize as usize,
                );
                let token = std::str::from_utf8(bytes)
                    .map_err(|_| "Saved API token has invalid encoding".to_string())?;
                tokens.push(token.to_string());
            }
            Ok(shared_legacy_account(&tokens))
        })();
        CredFree(credentials.cast());
        result
    }
}

fn optional_legacy_absence(error: Option<i32>) -> bool {
    matches!(error, Some(1168 | 1312))
}

fn shared_legacy_account(tokens: &[String]) -> Option<String> {
    let first = tokens.first()?;
    tokens
        .iter()
        .all(|token| token == first)
        .then(|| first.clone())
}

#[cfg(test)]
pub(crate) fn write_fixture(endpoint: &str, token: &str) -> bool {
    match write(endpoint, token) {
        Ok(()) => true,
        Err(error) if error.contains("(os error 1312)") => {
            eprintln!(
                "UNSUPPORTED: Windows Credential Manager integration requires a logon session with credential persistence (OS 1312); pure credential selection tests still run"
            );
            false
        }
        Err(error) => panic!("Synthetic credential fixture failed: {error}"),
    }
}

/// Forget applies to the installation's old project keys as well, so an old
/// entry cannot silently restore a token the user explicitly removed.
pub fn delete_legacy(base: &str, project_segment: &str) -> Result<(), String> {
    if base.contains('*') {
        return Ok(());
    }
    let prefix = format!("git-rustle:api:{base}/{project_segment}/");
    let filter: Vec<u16> = format!("{prefix}*\0").encode_utf16().collect();
    let mut count = 0;
    let mut credentials = std::ptr::null_mut();
    // SAFETY: enumerate owns every target string until the single CredFree.
    // CredDelete copies the target synchronously and does not free this array.
    unsafe {
        if CredEnumerateW(filter.as_ptr(), 0, &mut count, &mut credentials) == 0 {
            let error = std::io::Error::last_os_error();
            return if error.raw_os_error() == Some(1168) {
                Ok(())
            } else {
                Err(format!("Cannot enumerate legacy credentials: {error}"))
            };
        }
        let result = (|| {
            for ptr in std::slice::from_raw_parts(credentials, count as usize) {
                if CredDeleteW((**ptr).TargetName, CRED_TYPE_GENERIC, 0) == 0 {
                    let error = std::io::Error::last_os_error();
                    if error.raw_os_error() != Some(1168) {
                        return Err(format!("Cannot remove legacy API token: {error}"));
                    }
                }
            }
            Ok(())
        })();
        CredFree(credentials.cast());
        result
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn independent_legacy_accounts_never_bleed_into_another_project() {
        assert!(super::shared_legacy_account(&[]).is_none());
        assert_eq!(
            super::shared_legacy_account(&["fake-account".into(), "fake-account".into()])
                .as_deref(),
            Some("fake-account")
        );
        assert!(
            super::shared_legacy_account(&["fake-account-a".into(), "fake-account-b".into()])
                .is_none()
        );
    }
    #[test]
    fn unavailable_legacy_enumeration_does_not_block_settings_but_unexpected_errors_remain_errors()
    {
        assert!(super::optional_legacy_absence(Some(1312)));
        assert!(super::optional_legacy_absence(Some(1168)));
        assert!(!super::optional_legacy_absence(Some(5)));
        assert!(!super::optional_legacy_absence(None));
    }
    #[test]
    fn windows_token_round_trip_is_scoped_and_removable() {
        let dir = tempfile::tempdir().unwrap();
        let endpoint = format!(
            "https://example.invalid/test/{}",
            dir.path().file_name().unwrap().to_string_lossy()
        );
        struct Cleanup(String);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = super::delete(&self.0);
            }
        }
        let _cleanup = Cleanup(endpoint.clone());
        if !super::write_fixture(&endpoint, "synthetic-test-token") {
            return;
        }
        assert_eq!(
            super::read(&endpoint).unwrap().as_deref(),
            Some("synthetic-test-token")
        );
        assert_eq!(super::read(&format!("{endpoint}/other")).unwrap(), None);
        super::delete(&endpoint).unwrap();
        assert_eq!(super::read(&endpoint).unwrap(), None);
    }
}
