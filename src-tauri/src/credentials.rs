use zeroize::Zeroizing;

#[cfg(windows)]
mod windows {
    use super::*;
    use windows_sys::Win32::{
        Foundation::{GetLastError, ERROR_CANCELLED, ERROR_NOT_FOUND},
        Security::Credentials::*,
    };
    const TARGET: &str = "net.chuckdudley.mdash/openrouter";
    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain([0]).collect()
    }

    pub fn prompt(parent: isize) -> Result<Option<Zeroizing<String>>, String> {
        let mut name = wide("OpenRouter management key");
        name.resize(513, 0);
        let mut password = Zeroizing::new(vec![0_u16; 513]);
        let caption = wide("Connect OpenRouter to m-dash");
        let message = wide("Paste your OpenRouter MANAGEMENT KEY into Password. m-dash validates it, then stores it in Windows Credential Manager. No model requests are made.");
        let target = wide(TARGET);
        let info = CREDUI_INFOW {
            cbSize: std::mem::size_of::<CREDUI_INFOW>() as u32,
            hwndParent: parent as _,
            pszMessageText: message.as_ptr(),
            pszCaptionText: caption.as_ptr(),
            ..Default::default()
        };
        let mut save = 0;
        // Buffers and UI strings remain alive for the synchronous Windows call.
        let status = unsafe {
            CredUIPromptForCredentialsW(
                &info,
                target.as_ptr(),
                std::ptr::null(),
                0,
                name.as_mut_ptr(),
                name.len() as u32,
                password.as_mut_ptr(),
                password.len() as u32,
                &mut save,
                CREDUI_FLAGS_GENERIC_CREDENTIALS
                    | CREDUI_FLAGS_ALWAYS_SHOW_UI
                    | CREDUI_FLAGS_DO_NOT_PERSIST
                    | CREDUI_FLAGS_KEEP_USERNAME,
            )
        };
        if status == ERROR_CANCELLED {
            return Ok(None);
        }
        if status != 0 {
            return Err("Windows could not open the secure key prompt.".into());
        }
        let end = password
            .iter()
            .position(|n| *n == 0)
            .unwrap_or(password.len());
        let key = Zeroizing::new(
            String::from_utf16(&password[..end])
                .map_err(|_| "The management key contains invalid text.")?,
        );
        if !key.starts_with("sk-or-") || key.len() < 20 || key.chars().any(char::is_whitespace) {
            return Err("Enter an OpenRouter management key, without spaces.".into());
        }
        Ok(Some(key))
    }

    pub(crate) fn read_target(target: &str) -> Result<Option<Zeroizing<String>>, String> {
        let target = wide(target);
        let mut credential = std::ptr::null_mut();
        if unsafe { CredReadW(target.as_ptr(), CRED_TYPE_GENERIC, 0, &mut credential) } == 0 {
            return if unsafe { GetLastError() } == ERROR_NOT_FOUND {
                Ok(None)
            } else {
                Err("Windows Credential Manager could not be read.".into())
            };
        }
        // CredReadW owns this allocation until CredFree; copy only the credential blob.
        let result = unsafe {
            let value = &*credential;
            let result = if value.CredentialBlobSize == 0
                || value.CredentialBlobSize > CRED_MAX_CREDENTIAL_BLOB_SIZE
            {
                Err("The saved credential is invalid.".into())
            } else {
                let bytes = std::slice::from_raw_parts(
                    value.CredentialBlob,
                    value.CredentialBlobSize as usize,
                );
                std::str::from_utf8(bytes)
                    .map(|s| Some(Zeroizing::new(s.to_owned())))
                    .map_err(|_| "The saved credential is invalid.".into())
            };
            CredFree(credential.cast());
            result
        };
        result
    }
    pub(crate) fn write_target(target: &str, key: &str) -> Result<(), String> {
        if key.is_empty() || key.len() > CRED_MAX_CREDENTIAL_BLOB_SIZE as usize {
            return Err(
                "Credential exceeds Windows secure storage limits; saved data was not changed."
                    .into(),
            );
        }
        let mut target = wide(target);
        let mut name = wide("m-dash");
        let credential = CREDENTIALW {
            Type: CRED_TYPE_GENERIC,
            TargetName: target.as_mut_ptr(),
            UserName: name.as_mut_ptr(),
            CredentialBlobSize: key.len() as u32,
            CredentialBlob: key.as_ptr() as _,
            Persist: CRED_PERSIST_LOCAL_MACHINE,
            ..Default::default()
        };
        if unsafe { CredWriteW(&credential, 0) } == 0 {
            Err("Windows Credential Manager could not save the credential; saved data was not changed.".into())
        } else {
            Ok(())
        }
    }
    pub(crate) fn delete_target(target: &str) -> Result<(), String> {
        let target = wide(target);
        if unsafe { CredDeleteW(target.as_ptr(), CRED_TYPE_GENERIC, 0) } == 0
            && unsafe { GetLastError() } != ERROR_NOT_FOUND
        {
            Err("Windows Credential Manager could not remove the key.".into())
        } else {
            Ok(())
        }
    }
    pub fn read() -> Result<Option<Zeroizing<String>>, String> {
        read_target(TARGET)
    }
    pub fn write(key: &str) -> Result<(), String> {
        write_target(TARGET, key)
    }
    pub fn delete() -> Result<(), String> {
        delete_target(TARGET)
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        #[test]
        fn windows_vault_round_trip_uses_an_isolated_nonsecret_test_entry() {
            let target = format!("{TARGET}/test/{}", uuid::Uuid::new_v4());
            assert!(read_target(&target).unwrap().is_none());
            write_target(&target, "nonsecret-test-marker").unwrap();
            let read = read_target(&target);
            let deleted = delete_target(&target);
            assert_eq!(read.unwrap().unwrap().as_str(), "nonsecret-test-marker");
            deleted.unwrap();
            assert!(read_target(&target).unwrap().is_none());
        }
    }
}
#[cfg(windows)]
pub use windows::*;

#[cfg(target_os = "macos")]
mod macos {
    use super::*;
    use security_framework::passwords::{
        delete_generic_password, get_generic_password, set_generic_password,
    };
    #[cfg(not(test))]
    const SERVICE: &str = "net.chuckdudley.mdash/openrouter";
    #[cfg(test)]
    const SERVICE: &str = "net.chuckdudley.mdash/openrouter/nonsecret-test";
    const ACCOUNT: &str = "OpenRouter management key";
    const NOT_FOUND: i32 = -25300;

    pub fn read() -> Result<Option<Zeroizing<String>>, String> {
        let bytes = match get_generic_password(SERVICE, ACCOUNT) {
            Ok(bytes) => Zeroizing::new(bytes),
            Err(error) if error.code() == NOT_FOUND => return Ok(None),
            Err(_) => return Err("macOS Keychain could not read the OpenRouter key. Unlock your login Keychain and retry.".into()),
        };
        if bytes.is_empty() || bytes.len() > 4096 {
            return Err("The saved OpenRouter credential is invalid.".into());
        }
        std::str::from_utf8(&bytes)
            .map(|key| Some(Zeroizing::new(key.to_owned())))
            .map_err(|_| "The saved OpenRouter credential is invalid.".into())
    }
    pub fn write(key: &str) -> Result<(), String> {
        set_generic_password(SERVICE, ACCOUNT, key.as_bytes()).map_err(|_| {
            "macOS Keychain could not save the key; the connection was not changed.".into()
        })
    }
    pub fn delete() -> Result<(), String> {
        match delete_generic_password(SERVICE, ACCOUNT) {
            Ok(()) => Ok(()),
            Err(error) if error.code() == NOT_FOUND => Ok(()),
            Err(_) => Err("macOS Keychain could not remove the OpenRouter key.".into()),
        }
    }
    pub fn prompt(_: isize) -> Result<Option<Zeroizing<String>>, String> {
        use objc2::MainThreadMarker;
        use objc2_app_kit::{NSAlert, NSAlertFirstButtonReturn, NSSecureTextField};
        use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};
        let mtm = MainThreadMarker::new().ok_or("The secure prompt requires the main thread.")?;
        let alert = NSAlert::new(mtm);
        alert.setMessageText(&NSString::from_str("Connect OpenRouter to m-dash"));
        alert.setInformativeText(&NSString::from_str("Enter your OpenRouter management key. m-dash validates it and stores it in macOS Keychain. Management keys have broader authority; m-dash only reads credits and activity."));
        alert.addButtonWithTitle(&NSString::from_str("Connect"));
        alert.addButtonWithTitle(&NSString::from_str("Cancel"));
        let field = NSSecureTextField::new(mtm);
        field.setFrame(NSRect::new(
            NSPoint::new(0.0, 0.0),
            NSSize::new(360.0, 24.0),
        ));
        field.setPlaceholderString(Some(&NSString::from_str("OpenRouter management key")));
        alert.setAccessoryView(Some(&field));
        alert.window().setInitialFirstResponder(Some(&field));
        let accepted = alert.runModal() == NSAlertFirstButtonReturn;
        let key = Zeroizing::new(field.stringValue().to_string());
        field.setStringValue(&NSString::from_str(""));
        if !accepted {
            return Ok(None);
        }
        if !key.starts_with("sk-or-")
            || key.len() < 20
            || key.len() > 4096
            || key.chars().any(char::is_whitespace)
        {
            return Err("Enter an OpenRouter management key, without spaces.".into());
        }
        Ok(Some(key))
    }
}
#[cfg(target_os = "macos")]
pub use macos::*;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::{delete, prompt, read, write};

#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
pub fn read() -> Result<Option<Zeroizing<String>>, String> {
    Ok(None)
}
#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
pub fn prompt(_: isize) -> Result<Option<Zeroizing<String>>, String> {
    Err("Secure key setup is unavailable on this platform.".into())
}
#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
pub fn write(_: &str) -> Result<(), String> {
    Err("Secure key storage is unavailable on this platform.".into())
}
#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
pub fn delete() -> Result<(), String> {
    Err("Secure key storage is unavailable on this platform.".into())
}

#[cfg(all(test, target_os = "macos"))]
mod mac_tests {
    use super::*;
    #[test]
    #[ignore = "requires the real login Keychain; uses a separate nonsecret test service"]
    fn mac_keychain_preserves_replaces_and_removes_a_key() {
        // Mac test builds must use a service distinct from the production credential.
        assert!(read().unwrap().is_none());
        write("nonsecret-test-marker").unwrap();
        let first = read().unwrap().unwrap();
        write("nonsecret-replacement").unwrap();
        let second = read().unwrap().unwrap();
        delete().unwrap();
        assert_eq!(first.as_str(), "nonsecret-test-marker");
        assert_eq!(second.as_str(), "nonsecret-replacement");
        assert!(read().unwrap().is_none());
        delete().unwrap();
    }
}

#[cfg(all(test, target_os = "linux"))]
mod linux_tests {
    #[test]
    fn unavailable_service_is_an_error_without_a_plaintext_fallback() {
        if std::env::var_os("MDASH_TEST_NO_SECRET_SERVICE").is_some() {
            assert!(super::read().is_err());
            assert!(super::write("nonsecret-test-marker").is_err());
            assert!(super::delete().is_err());
            return;
        }
        assert!(std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "credentials::linux_tests::unavailable_service_is_an_error_without_a_plaintext_fallback"])
            .env("MDASH_TEST_NO_SECRET_SERVICE", "1")
            .env("DBUS_SESSION_BUS_ADDRESS", "unix:path=/nonexistent-m-dash-test-bus")
            .status().unwrap().success());
    }
}
