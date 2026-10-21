//! Use the desktop's maintained libsecret client; never fall back to a file.
use super::Zeroizing;
use std::{
    ffi::{c_char, c_int, c_void, CStr, CString},
    io::Read,
    process::{Command, Stdio},
    ptr::{null, null_mut},
};

const TARGET: &str = "net.chuckdudley.mdash/openrouter";
const STORAGE_ERROR: &str = "Desktop Secret Service could not access the OpenRouter key. Unlock your keyring and retry; the saved connection was not changed.";

#[repr(C)]
struct List {
    data: *mut c_void,
    next: *mut List,
    prev: *mut List,
}

// libsecret's stable C ABI owns all returned objects; the guards below release them.
#[link(name = "secret-1")]
unsafe extern "C" {
    fn secret_schema_new(name: *const c_char, flags: c_int, ...) -> *mut c_void;
    fn secret_schema_unref(schema: *mut c_void);
    fn secret_password_search_sync(
        schema: *mut c_void,
        flags: c_int,
        cancellable: *mut c_void,
        error: *mut *mut c_void,
        ...
    ) -> *mut List;
    fn secret_password_store_sync(
        schema: *mut c_void,
        collection: *const c_char,
        label: *const c_char,
        password: *const c_char,
        cancellable: *mut c_void,
        error: *mut *mut c_void,
        ...
    ) -> c_int;
    fn secret_password_clear_sync(
        schema: *mut c_void,
        cancellable: *mut c_void,
        error: *mut *mut c_void,
        ...
    ) -> c_int;
    fn secret_item_get_locked(item: *mut c_void) -> c_int;
    fn secret_item_get_secret(item: *mut c_void) -> *mut c_void;
    fn secret_value_get_text(value: *mut c_void) -> *const c_char;
    fn secret_value_unref(value: *mut c_void);
}
#[link(name = "glib-2.0")]
unsafe extern "C" {
    fn g_error_free(error: *mut c_void);
    fn g_list_free_full(list: *mut List, destroy: Option<unsafe extern "C" fn(*mut c_void)>);
}
#[link(name = "gobject-2.0")]
unsafe extern "C" {
    fn g_object_unref(object: *mut c_void);
}

struct Entry {
    schema: *mut c_void,
    items: *mut List,
    target: CString,
}

impl Drop for Entry {
    fn drop(&mut self) {
        unsafe {
            g_list_free_full(self.items, Some(g_object_unref));
            secret_schema_unref(self.schema);
        }
    }
}

fn check_error(error: *mut c_void) -> Result<(), String> {
    if error.is_null() {
        Ok(())
    } else {
        unsafe { g_error_free(error) };
        Err(STORAGE_ERROR.into())
    }
}

impl Entry {
    fn find(target: &str, load: bool) -> Result<Self, String> {
        let target = CString::new(target).map_err(|_| "Invalid credential target.")?;
        let schema = unsafe {
            secret_schema_new(
                c"net.chuckdudley.mdash.OpenRouter".as_ptr(),
                0,
                c"target".as_ptr(),
                0 as c_int,
                null::<c_char>(),
            )
        };
        if schema.is_null() {
            return Err(STORAGE_ERROR.into());
        }
        let mut entry = Self {
            schema,
            items: null_mut(),
            target,
        };
        let mut error = null_mut();
        // Search all matches without unlocking. A cancelled/locked keyring must not look unconfigured.
        entry.items = unsafe {
            secret_password_search_sync(
                schema,
                2 | if load { 8 } else { 0 },
                null_mut(),
                &mut error,
                c"target".as_ptr(),
                entry.target.as_ptr(),
                null::<c_char>(),
            )
        };
        check_error(error)?;
        let mut item = entry.items;
        while !item.is_null() {
            if unsafe { secret_item_get_locked((*item).data) } != 0 {
                return Err(STORAGE_ERROR.into());
            }
            item = unsafe { (*item).next };
        }
        Ok(entry)
    }
}

fn read_target(target: &str) -> Result<Option<Zeroizing<String>>, String> {
    let entry = Entry::find(target, true)?;
    if entry.items.is_null() {
        return Ok(None);
    }
    if unsafe { !(*entry.items).next.is_null() } {
        return Err("Multiple OpenRouter credentials were found. Resolve them in your keyring before reconnecting; nothing was changed.".into());
    }
    let value = unsafe { secret_item_get_secret((*entry.items).data) };
    if value.is_null() {
        return Err(STORAGE_ERROR.into());
    }
    let text = unsafe { secret_value_get_text(value) };
    let result = if text.is_null() {
        Err("The saved OpenRouter credential is invalid.".into())
    } else {
        let bytes = unsafe { CStr::from_ptr(text) }.to_bytes();
        if bytes.is_empty() || bytes.len() > 4096 {
            Err("The saved OpenRouter credential is invalid.".into())
        } else {
            std::str::from_utf8(bytes)
                .map(|s| Some(Zeroizing::new(s.to_owned())))
                .map_err(|_| "The saved OpenRouter credential is invalid.".into())
        }
    };
    unsafe { secret_value_unref(value) };
    result
}

fn write_target(target: &str, key: &str) -> Result<(), String> {
    if key.is_empty() || key.len() > 4096 || key.contains('\0') {
        return Err("Invalid credential; the saved connection was not changed.".into());
    }
    let entry = Entry::find(target, false)?;
    let mut password = Zeroizing::new(key.as_bytes().to_vec());
    password.push(0);
    let mut error = null_mut();
    let saved = unsafe {
        secret_password_store_sync(
            entry.schema,
            c"default".as_ptr(),
            c"m-dash OpenRouter management key".as_ptr(),
            password.as_ptr().cast(),
            null_mut(),
            &mut error,
            c"target".as_ptr(),
            entry.target.as_ptr(),
            null::<c_char>(),
        )
    };
    check_error(error)?;
    if saved == 0 {
        Err(STORAGE_ERROR.into())
    } else {
        Ok(())
    }
}

fn delete_target(target: &str) -> Result<(), String> {
    let entry = Entry::find(target, false)?;
    if entry.items.is_null() {
        return Ok(());
    }
    let mut error = null_mut();
    unsafe {
        secret_password_clear_sync(
            entry.schema,
            null_mut(),
            &mut error,
            c"target".as_ptr(),
            entry.target.as_ptr(),
            null::<c_char>(),
        );
    }
    check_error(error)?;
    if Entry::find(target, false)?.items.is_null() {
        Ok(())
    } else {
        Err(STORAGE_ERROR.into())
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

fn prompt_result(
    code: Option<i32>,
    mut bytes: Zeroizing<Vec<u8>>,
) -> Result<Option<Zeroizing<String>>, String> {
    if code == Some(1) {
        return Ok(None);
    }
    if code != Some(0) {
        return Err(
            "Could not open the native secure key prompt. Install zenity and retry.".into(),
        );
    }
    if bytes.last() == Some(&b'\n') {
        bytes.pop();
    }
    let key =
        std::str::from_utf8(&bytes).map_err(|_| "The management key contains invalid text.")?;
    if !key.starts_with("sk-or-")
        || key.len() < 20
        || key.len() > 4096
        || key.chars().any(|c| c.is_whitespace() || c == '\0')
    {
        return Err("Enter an OpenRouter management key, without spaces.".into());
    }
    Ok(Some(Zeroizing::new(key.to_owned())))
}

pub fn prompt(_: isize) -> Result<Option<Zeroizing<String>>, String> {
    // Zenity owns its GTK thread. Only a bounded private pipe carries the key to native Rust.
    let mut child = Command::new("/usr/bin/zenity")
        .args(["--password", "--title=m-dash · OpenRouter management key", "--text=Enter your OpenRouter management key. m-dash validates it before saving in your desktop keyring. No model requests are made."])
        .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null())
        .spawn().map_err(|_| "Could not open the native secure key prompt. Install zenity and retry.")?;
    let mut bytes = Zeroizing::new(Vec::new());
    let result = child
        .stdout
        .take()
        .ok_or("Could not open the secure prompt pipe.")?
        .take(4098)
        .read_to_end(&mut bytes);
    if result.is_err() || bytes.len() >= 4098 {
        let _ = child.kill();
        let _ = child.wait();
        return Err(
            "Could not read a bounded management key; the saved connection was not changed.".into(),
        );
    }
    let status = child.wait().map_err(|_| "The secure key prompt stopped.")?;
    prompt_result(status.code(), bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn secure_prompt_handles_cancel_failure_and_invalid_input() {
        assert!(prompt_result(
            Some(1),
            Zeroizing::new(b"sk-or-not-saved-on-cancel".to_vec())
        )
        .unwrap()
        .is_none());
        for (status, bytes) in [
            (Some(255), b"".as_slice()),
            (None, b""),
            (Some(0), b"short\n"),
            (Some(0), b"sk-or-contains whitespace\n"),
            (Some(0), b"sk-or-contains-a-nul\0\n"),
            (Some(0), b"\xff"),
        ] {
            assert!(prompt_result(status, Zeroizing::new(bytes.to_vec())).is_err());
        }
        assert!(prompt_result(Some(0), Zeroizing::new(vec![b'x'; 4098])).is_err());
        assert_eq!(
            prompt_result(
                Some(0),
                Zeroizing::new(b"sk-or-nonsecret-test-marker\n".to_vec())
            )
            .unwrap()
            .unwrap()
            .as_str(),
            "sk-or-nonsecret-test-marker"
        );
    }

    #[test]
    #[ignore = "requires a desktop Secret Service; uses an isolated nonsecret entry"]
    fn secret_service_preserves_replaces_and_removes_an_isolated_entry() {
        let target = format!("{TARGET}/test/{}", uuid::Uuid::new_v4());
        assert!(read_target(&target).unwrap().is_none());
        write_target(&target, "nonsecret-test-marker").unwrap();
        assert_eq!(
            read_target(&target).unwrap().unwrap().as_str(),
            "nonsecret-test-marker"
        );
        assert!(write_target(&target, "invalid\0replacement").is_err());
        assert_eq!(
            read_target(&target).unwrap().unwrap().as_str(),
            "nonsecret-test-marker"
        );
        write_target(&target, "nonsecret-replacement").unwrap();
        assert_eq!(
            read_target(&target).unwrap().unwrap().as_str(),
            "nonsecret-replacement"
        );
        delete_target(&target).unwrap();
        assert!(read_target(&target).unwrap().is_none());
        delete_target(&target).unwrap();
    }

    #[test]
    #[ignore = "run only under dbus-run-session with a disposable HOME and keyring"]
    fn locked_isolated_keyring_cannot_read_replace_or_delete_a_saved_connection() {
        assert_eq!(std::env::var("MDASH_ISOLATED_KEYRING").as_deref(), Ok("1"));
        assert_eq!(
            std::env::var_os("XDG_RUNTIME_DIR").map(std::path::PathBuf::from),
            std::env::var_os("HOME").map(|home| std::path::PathBuf::from(home).join("runtime")),
            "Use a disposable runtime directory as well as a disposable HOME and D-Bus session"
        );
        let target = format!("{TARGET}/test/{}", uuid::Uuid::new_v4());
        write_target(&target, "nonsecret-test-marker").unwrap();
        let locked = Command::new("/usr/bin/gdbus")
            .args([
                "call",
                "--session",
                "--dest",
                "org.freedesktop.secrets",
                "--object-path",
                "/org/freedesktop/secrets",
                "--method",
                "org.freedesktop.Secret.Service.Lock",
                "['/org/freedesktop/secrets/collection/login']",
            ])
            .output()
            .unwrap();
        assert!(locked.status.success());
        assert!(read_target(&target).is_err());
        assert!(write_target(&target, "nonsecret-replacement").is_err());
        assert!(delete_target(&target).is_err());
    }
}
