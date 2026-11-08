use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct ModulesConfig {
    #[serde(default)]
    modules: BTreeMap<String, bool>,
    #[serde(default)]
    service: BTreeMap<String, bool>,
}

pub fn check(modules_path: &Path, manifest_path: &Path) -> Result<(), Vec<String>> {
    let modules_src = std::fs::read_to_string(modules_path)
        .map_err(|e| vec![format!("failed to read {}: {e}", modules_path.display())])?;
    let manifest_src = std::fs::read_to_string(manifest_path)
        .map_err(|e| vec![format!("failed to read {}: {e}", manifest_path.display())])?;

    let modules: ModulesConfig = toml::from_str(&modules_src)
        .map_err(|e| vec![format!("failed to parse {}: {e}", modules_path.display())])?;
    let manifest: toml::Value = toml::from_str(&manifest_src)
        .map_err(|e| vec![format!("failed to parse {}: {e}", manifest_path.display())])?;

    let features = manifest
        .get("features")
        .and_then(|v| v.as_table())
        .cloned()
        .unwrap_or_default();
    let default = features
        .get("default")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect::<BTreeSet<_>>()
        })
        .unwrap_or_default();

    let mut errors = Vec::new();

    for (key, &enabled) in &modules.modules {
        validate(
            &features,
            &default,
            &format!("module-{key}"),
            enabled,
            &mut errors,
        );
    }
    for (key, &enabled) in &modules.service {
        validate(
            &features,
            &default,
            &format!("service-{key}"),
            enabled,
            &mut errors,
        );
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

fn validate(
    features: &toml::Table,
    default: &BTreeSet<String>,
    feature: &str,
    enabled: bool,
    errors: &mut Vec<String>,
) {
    if !features.contains_key(feature) {
        errors.push(format!("missing feature `{feature}` in mblinkd manifest"));
        return;
    }

    let in_default = default.contains(feature);
    if enabled && !in_default {
        errors.push(format!(
            "feature `{feature}` default mismatch: expected enabled=true"
        ));
    } else if !enabled && in_default {
        errors.push(format!(
            "feature `{feature}` default mismatch: expected enabled=false"
        ));
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::Write;
    use tempfile::TempDir;

    use crate::modules_check::check;

    fn write_file(dir: &TempDir, name: &str, contents: &str) {
        let path = dir.path().join(name);
        let mut file = fs::File::create(&path).unwrap();
        file.write_all(contents.as_bytes()).unwrap();
    }

    #[test]
    fn valid_pair_returns_ok() {
        let dir = TempDir::new().unwrap();
        write_file(
            &dir,
            "modules.toml",
            r#"
[modules]
notisync = false

[service]
systemd = true
openrc = false
"#,
        );
        write_file(
            &dir,
            "Cargo.toml",
            r#"
[features]
default = ["service-systemd"]
module-notisync = []
service-systemd = []
service-openrc = []
"#,
        );
        assert!(
            check(
                &dir.path().join("modules.toml"),
                &dir.path().join("Cargo.toml")
            )
            .is_ok()
        );
    }

    #[test]
    fn missing_module_feature_is_reported() {
        let dir = TempDir::new().unwrap();
        write_file(
            &dir,
            "modules.toml",
            r#"
[modules]
foo = true
"#,
        );
        write_file(
            &dir,
            "Cargo.toml",
            r#"
[features]
default = []
"#,
        );
        let err = check(
            &dir.path().join("modules.toml"),
            &dir.path().join("Cargo.toml"),
        )
        .unwrap_err();
        assert!(err.iter().any(|e| e.contains("module-foo")));
    }

    #[test]
    fn default_membership_mismatch_is_reported() {
        let dir = TempDir::new().unwrap();
        write_file(
            &dir,
            "modules.toml",
            r#"
[service]
systemd = true
"#,
        );
        write_file(
            &dir,
            "Cargo.toml",
            r#"
[features]
default = []
service-systemd = []
"#,
        );
        let err = check(
            &dir.path().join("modules.toml"),
            &dir.path().join("Cargo.toml"),
        )
        .unwrap_err();
        assert!(
            err.iter()
                .any(|e| e.contains("service-systemd") && e.contains("true"))
        );
    }
}
