/// Build script: record the jj-lib version Cargo.lock builds against, and
/// warn if vendored jj-cli config files are stale relative to it.
fn main() {
    println!("cargo:rerun-if-changed=vendored/revsets.toml");
    println!("cargo:rerun-if-changed=Cargo.lock");

    let cargo_lock: toml::Table = std::fs::read_to_string("Cargo.lock")
        .expect("failed to read Cargo.lock")
        .parse()
        .expect("failed to parse Cargo.lock");

    let revsets_toml =
        std::fs::read_to_string("vendored/revsets.toml").expect("failed to read revsets.toml");

    let locked = locked_jj_lib_version(&cargo_lock);
    if let Some(version) = &locked {
        // What `JjVersion::linked` reads, to compare the `jj` binary against.
        println!("cargo:rustc-env=KOJUTSU_JJ_LIB_VERSION={version}");
    }

    match (locked, extract_vendored_version(&revsets_toml)) {
        (Some(locked), Some(vendored)) if locked != vendored => {
            println!(
                "cargo:warning=Vendored revsets.toml is for jj v{vendored}, \
                 but Cargo.lock builds against jj-lib v{locked}. \
                 Run `scripts/update-jj-config.sh` to update."
            );
        }
        (Some(_), Some(_)) => {}
        (None, _) => {
            println!("cargo:warning=Could not find jj-lib in Cargo.lock");
        }
        (_, None) => {
            println!(
                "cargo:warning=Could not detect jj-version from vendored/revsets.toml. \
                 Run `scripts/update-jj-config.sh` to regenerate."
            );
        }
    }
}

/// The exact jj-lib version Cargo.lock pins.
fn locked_jj_lib_version(lock: &toml::Table) -> Option<String> {
    lock.get("package")?
        .as_array()?
        .iter()
        .filter_map(toml::Value::as_table)
        .find(|package| package.get("name").and_then(toml::Value::as_str) == Some("jj-lib"))?
        .get("version")?
        .as_str()
        .map(String::from)
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
