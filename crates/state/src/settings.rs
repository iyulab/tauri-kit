use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::{Map, Value};

/// An app's settings for this device, kept as one JSON object in a file — written crash-safely,
/// and without losing the fields this release does not know.
///
/// A newer release may add settings; an older one, run again after it, reads the file into its own
/// `T` and would drop the newer fields the first time it saves. `Settings` keeps them: the fields
/// of the file that `T` did not account for when it was read are written back beside `T`'s own.
/// (A field `T` knows is `T`'s: it is written as `T` says, or left out when `T` leaves it out.)
///
/// A file that is not a JSON object `T` can be read from is moved aside
/// ([`set_aside`](crate::set_aside) — `<name>.unreadable-<time>`), not lost, and the settings start
/// from `T::default()`; [`Settings::set_aside`] says where it went.
///
/// Its format over releases is the directory's: keep the file in a state directory and bring it
/// up to date with [`Versioned`](crate::Versioned) before loading it — and, when that directory's
/// format is unknown, load it and do not save.
///
/// ```
/// use serde::{Deserialize, Serialize};
/// use tauri_kit_state::Settings;
///
/// #[derive(Default, Serialize, Deserialize)]
/// #[serde(default)]
/// struct Prefs { zoom: u32 }
///
/// let dir = tempfile::tempdir()?;
/// let file = dir.path().join("settings.json");
/// std::fs::write(&file, r#"{"zoom":2,"theme":"dark"}"#)?; // "theme" from a newer release
/// let mut settings = Settings::<Prefs>::load(&file)?;
/// settings.value_mut().zoom = 3;
/// settings.save()?;
/// let saved: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&file)?)?;
/// assert_eq!(saved, serde_json::json!({ "zoom": 3, "theme": "dark" }));
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug)]
pub struct Settings<T> {
    file: PathBuf,
    value: T,
    unknown: Map<String, Value>,
    set_aside: Option<PathBuf>,
}

impl<T: Serialize + DeserializeOwned + Default> Settings<T> {
    /// The settings in `file`: `T::default()` when there is none yet, or when what is there cannot
    /// be read (it is moved aside first).
    pub fn load(file: &Path) -> io::Result<Self> {
        let text = match std::fs::read(file) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Self::fresh(file, None)),
            Err(e) => return Err(e),
        };
        let read = serde_json::from_slice::<Map<String, Value>>(&text)
            .ok()
            .and_then(|object| {
                let value = T::deserialize(Value::Object(object.clone())).ok()?;
                Some((object, value))
            });
        let Some((mut object, value)) = read else {
            return Ok(Self::fresh(file, Some(move_aside(file)?)));
        };
        // What `T` writes of itself is what it knows; the rest of the file is kept as it was.
        if let Value::Object(known) = serde_json::to_value(&value).map_err(io::Error::other)? {
            object.retain(|key, _| !known.contains_key(key));
        }
        Ok(Self {
            file: file.to_path_buf(),
            value,
            unknown: object,
            set_aside: None,
        })
    }

    fn fresh(file: &Path, set_aside: Option<PathBuf>) -> Self {
        Self {
            file: file.to_path_buf(),
            value: T::default(),
            unknown: Map::new(),
            set_aside,
        }
    }

    /// Writes the settings to the file they were loaded from, crash-safely: the fields of `T` and
    /// the fields this release did not know.
    pub fn save(&self) -> io::Result<()> {
        let mut object = self.unknown.clone();
        match serde_json::to_value(&self.value).map_err(io::Error::other)? {
            Value::Object(own) => object.extend(own),
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "settings must serialize as a JSON object",
                ))
            }
        }
        let mut text =
            serde_json::to_vec_pretty(&Value::Object(object)).map_err(io::Error::other)?;
        text.push(b'\n');
        tauri_kit_fs::write_atomic(&self.file, &text)
    }
}

impl<T> Settings<T> {
    /// The settings.
    pub fn value(&self) -> &T {
        &self.value
    }

    /// The settings, to change before [`save`](Settings::save).
    pub fn value_mut(&mut self) -> &mut T {
        &mut self.value
    }

    /// Where an unreadable file was moved when these settings were loaded, if one was.
    pub fn set_aside(&self) -> Option<&Path> {
        self.set_aside.as_deref()
    }

    /// The fields of the file this release does not know, as they were read.
    pub fn unknown(&self) -> &Map<String, Value> {
        &self.unknown
    }
}

fn move_aside(file: &Path) -> io::Result<PathBuf> {
    let dir = file.parent().unwrap_or(Path::new("."));
    let name = file
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "settings file name"))?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    crate::set_aside(dir, name, now)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;
    use serde_json::json;

    #[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
    #[serde(default)]
    struct Prefs {
        zoom: u32,
        #[serde(skip_serializing_if = "Option::is_none")]
        last: Option<String>,
    }

    fn written(file: &Path) -> Value {
        serde_json::from_str(&std::fs::read_to_string(file).unwrap()).unwrap()
    }

    #[test]
    fn no_file_is_the_defaults_and_saving_makes_one() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("deeper/settings.json");
        let mut s = Settings::<Prefs>::load(&file).unwrap();
        assert_eq!(s.value(), &Prefs::default());
        s.value_mut().zoom = 2;
        s.save().unwrap();
        assert_eq!(written(&file), json!({ "zoom": 2 }));
    }

    #[test]
    fn fields_from_a_newer_release_survive_a_save() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("settings.json");
        std::fs::write(&file, r#"{"zoom":1,"theme":{"name":"dark"},"last":"a"}"#).unwrap();
        let mut s = Settings::<Prefs>::load(&file).unwrap();
        assert_eq!(
            s.unknown(),
            json!({ "theme": { "name": "dark" } }).as_object().unwrap()
        );
        s.value_mut().zoom = 4;
        s.save().unwrap();
        assert_eq!(
            written(&file),
            json!({ "zoom": 4, "theme": { "name": "dark" }, "last": "a" })
        );
    }

    #[test]
    fn a_known_field_the_app_clears_is_gone_not_brought_back() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("settings.json");
        std::fs::write(&file, r#"{"zoom":1,"last":"a"}"#).unwrap();
        let mut s = Settings::<Prefs>::load(&file).unwrap();
        s.value_mut().last = None;
        s.save().unwrap();
        assert_eq!(written(&file), json!({ "zoom": 1 }));
    }

    #[test]
    fn an_unreadable_file_is_moved_aside_not_lost() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("settings.json");
        for bad in [&b"{not json"[..], br#"["a list"]"#, br#"{"zoom":"high"}"#] {
            std::fs::write(&file, bad).unwrap();
            let s = Settings::<Prefs>::load(&file).unwrap();
            assert_eq!(s.value(), &Prefs::default());
            let aside = s.set_aside().expect("moved aside");
            assert_eq!(std::fs::read(aside).unwrap(), bad);
            assert!(!file.exists());
        }
    }

    #[test]
    fn settings_that_are_not_an_object_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = Settings::<Vec<u32>>::load(&dir.path().join("s.json")).unwrap();
        s.value_mut().push(1);
        assert_eq!(s.save().unwrap_err().kind(), io::ErrorKind::InvalidInput);
    }
}
