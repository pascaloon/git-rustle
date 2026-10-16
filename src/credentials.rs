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

#[cfg(test)]
mod tests {
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
        assert_eq!(super::read(&endpoint).unwrap(), None);
        super::write(&endpoint, "synthetic-test-token").unwrap();
        assert_eq!(
            super::read(&endpoint).unwrap().as_deref(),
            Some("synthetic-test-token")
        );
        assert_eq!(super::read(&format!("{endpoint}/other")).unwrap(), None);
        super::delete(&endpoint).unwrap();
        assert_eq!(super::read(&endpoint).unwrap(), None);
    }
}
