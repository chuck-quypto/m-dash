use std::{
    fs,
    io::{Read, Write},
    path::Path,
};

const LIMIT: u64 = 131072;
const NAME: &str = "sample-preview-v1.json";
const ERROR: &str = "Sample storage unavailable";
fn validate(contents: &str) -> Result<(), String> {
    if contents.len() as u64 > LIMIT {
        return Err(ERROR.into());
    }
    let value: serde_json::Value = serde_json::from_str(contents).map_err(|_| ERROR)?;
    if value["mode"] != "sample" {
        return Err(ERROR.into());
    }
    Ok(())
}

pub fn read(directory: &Path) -> Result<Option<String>, String> {
    let file = match fs::File::open(directory.join(NAME)) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(ERROR.into()),
    };
    let mut contents = String::new();
    file.take(LIMIT + 1)
        .read_to_string(&mut contents)
        .map_err(|_| ERROR)?;
    validate(&contents)?;
    Ok(Some(contents))
}
pub fn write(directory: &Path, contents: &str) -> Result<(), String> {
    validate(contents)?;
    fs::create_dir_all(directory).map_err(|_| ERROR)?;
    let temporary = directory.join("sample-preview-v1.tmp");
    let mut file = fs::File::create(&temporary).map_err(|_| ERROR)?;
    file.write_all(contents.as_bytes()).map_err(|_| ERROR)?;
    file.sync_all().map_err(|_| ERROR)?;
    fs::rename(temporary, directory.join(NAME)).map_err(|_| ERROR)?;
    fs::File::open(directory)
        .and_then(|file| file.sync_all())
        .map_err(|_| ERROR)?;
    Ok(())
}
pub fn clear(directory: &Path) -> Result<(), String> {
    match fs::remove_file(directory.join(NAME)) {
        Ok(()) => fs::File::open(directory)
            .and_then(|file| file.sync_all())
            .map_err(|_| ERROR.into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(ERROR.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sample_store_survives_reopen_rejects_invalid_replacement_and_clears() {
        let directory = tempfile::tempdir().unwrap();
        let sample = r#"{"mode":"sample","snapshot":{"version":1},"receivedAt":100}"#;
        write(directory.path(), sample).unwrap();
        assert_eq!(read(directory.path()).unwrap().as_deref(), Some(sample));
        assert!(write(directory.path(), r#"{"mode":"live"}"#).is_err());
        assert!(write(directory.path(), &"x".repeat(131073)).is_err());
        assert_eq!(read(directory.path()).unwrap().as_deref(), Some(sample));
        clear(directory.path()).unwrap();
        assert_eq!(read(directory.path()).unwrap(), None);
    }
}
