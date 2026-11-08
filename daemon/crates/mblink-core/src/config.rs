use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

use mblink_proto::mblink::link::v1::{ConfigEntry as ProtoConfigEntry, ConfigMap as ProtoConfigMap};

/// A single configuration entry for a package.
///
/// `updated_at` is a unix-epoch millisecond timestamp.  `updated_by` is either
/// `"linux"` or `"phone"`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigEntry {
    pub package: String,
    pub enabled: bool,
    pub updated_at: u64,
    pub updated_by: String,
}

/// A map of package-name -> configuration entry.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ConfigMap {
    pub entries: BTreeMap<String, ConfigEntry>,
}

/// Serde representation used for TOML (de)serialization.
///
/// The package name is the table key, so it is not stored inside the entry
/// table itself.
#[derive(Debug, Serialize, Deserialize)]
struct ConfigMapRepr {
    #[serde(rename = "apps")]
    entries: BTreeMap<String, ConfigEntryRepr>,
}

#[derive(Debug, Serialize, Deserialize)]
struct ConfigEntryRepr {
    enabled: bool,
    updated_at: u64,
    updated_by: String,
}

impl ConfigMap {
    /// Serialize to the hand-editable TOML shape:
    ///
    /// ```toml
    /// [apps."com.google.android.gm"]
    /// enabled = true
    /// updated_at = 1760000000123
    /// updated_by = "linux"
    /// ```
    pub fn to_toml(&self) -> String {
        let repr = ConfigMapRepr {
            entries: self
                .entries
                .iter()
                .map(|(package, entry)| {
                    (
                        package.clone(),
                        ConfigEntryRepr {
                            enabled: entry.enabled,
                            updated_at: entry.updated_at,
                            updated_by: entry.updated_by.clone(),
                        },
                    )
                })
                .collect(),
        };
        toml::to_string_pretty(&repr).expect("ConfigMap serializes to TOML")
    }

    /// Deserialize from the hand-editable TOML shape.
    pub fn from_toml(s: &str) -> Result<Self, toml::de::Error> {
        let repr: ConfigMapRepr = toml::from_str(s)?;
        let mut entries = BTreeMap::new();
        for (package, entry_repr) in repr.entries {
            entries.insert(
                package.clone(),
                ConfigEntry {
                    package,
                    enabled: entry_repr.enabled,
                    updated_at: entry_repr.updated_at,
                    updated_by: entry_repr.updated_by,
                },
            );
        }
        Ok(ConfigMap { entries })
    }
}

impl From<ProtoConfigMap> for ConfigMap {
    fn from(proto: ProtoConfigMap) -> Self {
        let mut entries = BTreeMap::new();
        for entry in proto.entries {
            entries.insert(
                entry.package.clone(),
                ConfigEntry {
                    package: entry.package,
                    enabled: entry.enabled,
                    updated_at: entry.updated_at,
                    updated_by: entry.updated_by,
                },
            );
        }
        ConfigMap { entries }
    }
}

impl From<ConfigMap> for ProtoConfigMap {
    fn from(map: ConfigMap) -> Self {
        ProtoConfigMap {
            entries: map
                .entries
                .into_values()
                .map(|entry| ProtoConfigEntry {
                    package: entry.package,
                    enabled: entry.enabled,
                    updated_at: entry.updated_at,
                    updated_by: entry.updated_by,
                })
                .collect(),
        }
    }
}

/// Last-write-wins merge.
///
/// Per package:
/// - absent in `base` -> insert from `incoming`
/// - present in `base` -> `incoming` wins iff the tuple
///   `(updated_at, updated_by)` is strictly greater
/// - exact tie (`updated_at` and `updated_by` both equal) keeps `base`
pub fn merge_lww(base: &mut ConfigMap, incoming: &ConfigMap) {
    for (package, incoming_entry) in &incoming.entries {
        let should_insert = match base.entries.get(package) {
            Some(base_entry) => {
                let base_tuple = (base_entry.updated_at, &base_entry.updated_by);
                let incoming_tuple = (incoming_entry.updated_at, &incoming_entry.updated_by);
                incoming_tuple > base_tuple
            }
            None => true,
        };
        if should_insert {
            base.entries.insert(package.clone(), incoming_entry.clone());
        }
    }
}

/// Merge a config file into the current in-memory map using hand-edit
/// semantics.
///
/// For every entry in `file` whose `enabled` value differs from `current`, the
/// entry is treated as a local Linux edit: its `updated_at` is set to the file's
/// `mtime_ms` and `updated_by` is set to `"linux"`.  After that adjustment the
/// maps are merged with [`merge_lww`].
pub fn merge_from_file(current: &mut ConfigMap, file: &ConfigMap, mtime_ms: u64) {
    let mut adjusted = file.clone();
    for (package, file_entry) in &mut adjusted.entries {
        let current_enabled = current.entries.get(package).map(|e| e.enabled);
        if current_enabled != Some(file_entry.enabled) {
            file_entry.updated_at = mtime_ms;
            file_entry.updated_by = "linux".to_string();
        }
    }
    merge_lww(current, &adjusted);
}

/// Errors that can occur when loading a module config file.
#[derive(Debug)]
pub enum ConfigLoadError {
    Parse(toml::de::Error),
    Io(std::io::Error),
}

impl std::fmt::Display for ConfigLoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigLoadError::Parse(e) => write!(f, "failed to parse config: {e}"),
            ConfigLoadError::Io(e) => write!(f, "failed to read config: {e}"),
        }
    }
}

impl std::error::Error for ConfigLoadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ConfigLoadError::Parse(e) => Some(e),
            ConfigLoadError::Io(e) => Some(e),
        }
    }
}

/// Persistent store for module config files at `<root>/<module>/config.toml`.
#[derive(Debug, Clone)]
pub struct ConfigStore {
    root: PathBuf,
}

impl ConfigStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        ConfigStore { root: root.into() }
    }

    /// Return the path for a module's config file.
    pub fn path(&self, module: &str) -> PathBuf {
        self.root.join(module).join("config.toml")
    }

    /// Load a module's config file.  A missing file returns an empty
    /// [`ConfigMap`]; a parse failure returns [`ConfigLoadError::Parse`].
    pub fn load(&self, module: &str) -> Result<ConfigMap, ConfigLoadError> {
        let path = self.path(module);
        match fs::read_to_string(&path) {
            Ok(content) => ConfigMap::from_toml(&content).map_err(ConfigLoadError::Parse),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(ConfigMap::default()),
            Err(e) => Err(ConfigLoadError::Io(e)),
        }
    }

    /// Atomically save a module's config file.
    ///
    /// Writes to a uniquely-named temp file in the same directory, fsyncs it,
    /// then renames it into place.
    pub fn save(&self, module: &str, map: &ConfigMap) -> std::io::Result<()> {
        let path = self.path(module);
        let dir = path
            .parent()
            .expect("config.toml path always has a parent directory");
        fs::create_dir_all(dir)?;

        let tmp_name = format!("config.toml.tmp{}", tmp_suffix());
        let tmp_path = dir.join(&tmp_name);

        let mut file = fs::File::create(&tmp_path)?;
        file.write_all(map.to_toml().as_bytes())?;
        file.sync_all()?;
        drop(file);

        fs::rename(&tmp_path, &path)?;
        Ok(())
    }
}

static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

fn tmp_suffix() -> u64 {
    TMP_COUNTER.fetch_add(1, Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mblink_proto::mblink::link::v1::ConfigMap as ProtoConfigMap;

    #[test]
    fn toml_roundtrip() {
        let mut map = ConfigMap::default();
        map.entries.insert(
            "com.google.android.gm".to_string(),
            ConfigEntry {
                package: "com.google.android.gm".to_string(),
                enabled: true,
                updated_at: 1760000000123,
                updated_by: "linux".to_string(),
            },
        );
        let s = map.to_toml();
        let parsed = ConfigMap::from_toml(&s).unwrap();
        assert_eq!(map, parsed);
        assert!(s.contains("[apps.\"com.google.android.gm\"]"));
        assert!(s.contains("enabled = true"));
        assert!(s.contains("updated_at = 1760000000123"));
        assert!(s.contains("updated_by = \"linux\""));
    }

    #[test]
    fn merge_newer_wins() {
        let mut base = ConfigMap::default();
        base.entries.insert(
            "pkg".to_string(),
            ConfigEntry {
                package: "pkg".to_string(),
                enabled: true,
                updated_at: 100,
                updated_by: "linux".to_string(),
            },
        );
        let mut incoming = ConfigMap::default();
        incoming.entries.insert(
            "pkg".to_string(),
            ConfigEntry {
                package: "pkg".to_string(),
                enabled: false,
                updated_at: 200,
                updated_by: "linux".to_string(),
            },
        );
        merge_lww(&mut base, &incoming);
        assert_eq!(base.entries["pkg"].enabled, false);
        assert_eq!(base.entries["pkg"].updated_at, 200);
    }

    #[test]
    fn merge_older_ignored() {
        let mut base = ConfigMap::default();
        base.entries.insert(
            "pkg".to_string(),
            ConfigEntry {
                package: "pkg".to_string(),
                enabled: true,
                updated_at: 200,
                updated_by: "linux".to_string(),
            },
        );
        let mut incoming = ConfigMap::default();
        incoming.entries.insert(
            "pkg".to_string(),
            ConfigEntry {
                package: "pkg".to_string(),
                enabled: false,
                updated_at: 100,
                updated_by: "linux".to_string(),
            },
        );
        merge_lww(&mut base, &incoming);
        assert_eq!(base.entries["pkg"].enabled, true);
        assert_eq!(base.entries["pkg"].updated_at, 200);
    }

    #[test]
    fn merge_absent_inserted() {
        let mut base = ConfigMap::default();
        let mut incoming = ConfigMap::default();
        incoming.entries.insert(
            "pkg".to_string(),
            ConfigEntry {
                package: "pkg".to_string(),
                enabled: true,
                updated_at: 100,
                updated_by: "phone".to_string(),
            },
        );
        merge_lww(&mut base, &incoming);
        assert_eq!(base.entries["pkg"].enabled, true);
        assert_eq!(base.entries["pkg"].updated_by, "phone");
    }

    #[test]
    fn merge_tie_updated_at_resolved_by_updated_by() {
        let mut base = ConfigMap::default();
        base.entries.insert(
            "pkg".to_string(),
            ConfigEntry {
                package: "pkg".to_string(),
                enabled: true,
                updated_at: 100,
                updated_by: "linux".to_string(),
            },
        );
        let mut incoming = ConfigMap::default();
        incoming.entries.insert(
            "pkg".to_string(),
            ConfigEntry {
                package: "pkg".to_string(),
                enabled: false,
                updated_at: 100,
                updated_by: "phone".to_string(),
            },
        );
        merge_lww(&mut base, &incoming);
        assert_eq!(base.entries["pkg"].enabled, false);
        assert_eq!(base.entries["pkg"].updated_by, "phone");
    }

    #[test]
    fn merge_exact_tie_keeps_base() {
        let mut base = ConfigMap::default();
        base.entries.insert(
            "pkg".to_string(),
            ConfigEntry {
                package: "pkg".to_string(),
                enabled: true,
                updated_at: 100,
                updated_by: "linux".to_string(),
            },
        );
        let mut incoming = ConfigMap::default();
        incoming.entries.insert(
            "pkg".to_string(),
            ConfigEntry {
                package: "pkg".to_string(),
                enabled: false,
                updated_at: 100,
                updated_by: "linux".to_string(),
            },
        );
        merge_lww(&mut base, &incoming);
        assert_eq!(base.entries["pkg"].enabled, true);
    }

    #[test]
    fn merge_from_file_hand_edit_wins_with_mtime() {
        let mut current = ConfigMap::default();
        current.entries.insert(
            "pkg".to_string(),
            ConfigEntry {
                package: "pkg".to_string(),
                enabled: true,
                updated_at: 500,
                updated_by: "phone".to_string(),
            },
        );
        let mut file = ConfigMap::default();
        file.entries.insert(
            "pkg".to_string(),
            ConfigEntry {
                package: "pkg".to_string(),
                enabled: false,
                updated_at: 100, // stale
                updated_by: "phone".to_string(),
            },
        );
        merge_from_file(&mut current, &file, 600);
        assert_eq!(current.entries["pkg"].enabled, false);
        assert_eq!(current.entries["pkg"].updated_at, 600);
        assert_eq!(current.entries["pkg"].updated_by, "linux");
    }

    #[test]
    fn save_load_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let store = ConfigStore::new(dir.path());
        let mut map = ConfigMap::default();
        map.entries.insert(
            "com.google.android.gm".to_string(),
            ConfigEntry {
                package: "com.google.android.gm".to_string(),
                enabled: true,
                updated_at: 1760000000123,
                updated_by: "linux".to_string(),
            },
        );
        store.save("gm", &map).unwrap();
        let loaded = store.load("gm").unwrap();
        assert_eq!(map, loaded);
    }

    #[test]
    fn save_leaves_no_tmp_residue() {
        let dir = tempfile::tempdir().unwrap();
        let store = ConfigStore::new(dir.path());
        let mut map = ConfigMap::default();
        map.entries.insert(
            "pkg".to_string(),
            ConfigEntry {
                package: "pkg".to_string(),
                enabled: true,
                updated_at: 100,
                updated_by: "linux".to_string(),
            },
        );
        store.save("mod", &map).unwrap();
        let entries: Vec<_> = std::fs::read_dir(dir.path().join("mod")).unwrap().collect();
        assert_eq!(entries.len(), 1);
        assert!(entries[0].as_ref().unwrap().path().ends_with("config.toml"));
    }

    #[test]
    fn load_corrupt_toml_returns_parse_error() {
        let dir = tempfile::tempdir().unwrap();
        let store = ConfigStore::new(dir.path());
        let path = store.path("mod");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "this is not valid toml [[[").unwrap();
        let result = store.load("mod");
        assert!(matches!(result, Err(ConfigLoadError::Parse(_))));
    }

    #[test]
    fn load_missing_file_returns_empty_ok() {
        let dir = tempfile::tempdir().unwrap();
        let store = ConfigStore::new(dir.path());
        let loaded = store.load("nonexistent").unwrap();
        assert!(loaded.entries.is_empty());
    }

    #[test]
    fn proto_roundtrip() {
        let mut map = ConfigMap::default();
        map.entries.insert(
            "com.google.android.gm".to_string(),
            ConfigEntry {
                package: "com.google.android.gm".to_string(),
                enabled: true,
                updated_at: 1760000000123,
                updated_by: "linux".to_string(),
            },
        );
        let proto = ProtoConfigMap::from(map.clone());
        let back = ConfigMap::from(proto);
        assert_eq!(map, back);
    }
}
