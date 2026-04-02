/// Build script: warn if vendored jj-cli config files are stale relative to
/// the jj-lib version in Cargo.toml.
fn main() {
    println!("cargo:rerun-if-changed=vendored/revsets.toml");
    println!("cargo:rerun-if-changed=Cargo.toml");

    let cargo_toml: toml::Table = std::fs::read_to_string("Cargo.toml")
        .expect("failed to read Cargo.toml")
        .parse()
        .expect("failed to parse Cargo.toml");

    let revsets_toml =
        std::fs::read_to_string("vendored/revsets.toml").expect("failed to read revsets.toml");

    let cargo_version = extract_jj_lib_version(&cargo_toml);
    let vendored_version = extract_vendored_version(&revsets_toml);

    match (cargo_version, vendored_version) {
        (Some(cv), Some(vv)) => {
            let cv_normalized = normalize_version(&cv);
            let vv_normalized = normalize_version(&vv);
            if cv_normalized != vv_normalized {
                println!(
                    "cargo:warning=Vendored revsets.toml is for jj v{vv}, \
                     but Cargo.toml depends on jj-lib v{cv}. \
                     Run `scripts/update-jj-config.sh` to update."
                );
            }
        }
        (None, _) => {
            println!("cargo:warning=Could not detect jj-lib version from Cargo.toml");
        }
        (_, None) => {
            println!(
                "cargo:warning=Could not detect jj-version from vendored/revsets.toml. \
                 Run `scripts/update-jj-config.sh` to regenerate."
            );
        }
    }
}

/// Extract jj-lib version from parsed Cargo.toml.
fn extract_jj_lib_version(cargo: &toml::Table) -> Option<String> {
    let deps = cargo.get("dependencies")?.as_table()?;
    let jj_lib = deps.get("jj-lib")?;
    match jj_lib {
        toml::Value::String(v) => Some(v.clone()),
        toml::Value::Table(t) => t.get("version")?.as_str().map(String::from),
        _ => None,
    }
}

/// Extract `jj-version: X.Y.Z` from the vendored TOML header comments.
fn extract_vendored_version(toml: &str) -> Option<String> {
    for line in toml.lines() {
        if !line.starts_with('#') {
            break;
        }
        if let Some(rest) = line.strip_prefix("# jj-version:") {
            return Some(rest.trim().to_string());
        }
    }
    None
}

/// Normalize "0.39" to "0.39" and "0.39.0" to "0.39" for comparison.
fn normalize_version(v: &str) -> String {
    let parts: Vec<&str> = v.split('.').collect();
    match parts.as_slice() {
        [major, minor, patch] if *patch == "0" => format!("{major}.{minor}"),
        _ => v.to_string(),
    }
}
