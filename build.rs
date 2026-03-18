/// Build script: warn if vendored jj-cli config files are stale relative to
/// the jj-lib version in Cargo.toml.
fn main() {
    println!("cargo:rerun-if-changed=src/config/revsets.toml");
    println!("cargo:rerun-if-changed=Cargo.toml");

    let cargo_toml = std::fs::read_to_string("Cargo.toml").expect("failed to read Cargo.toml");
    let revsets_toml =
        std::fs::read_to_string("src/config/revsets.toml").expect("failed to read revsets.toml");

    let cargo_version = extract_jj_lib_version(&cargo_toml);
    let vendored_version = extract_vendored_version(&revsets_toml);

    match (cargo_version, vendored_version) {
        (Some(cv), Some(vv)) => {
            // Cargo.toml has "0.39" while vendored has "0.39.0" -- normalize
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
                "cargo:warning=Could not detect jj-version from src/config/revsets.toml. \
                 Run `scripts/update-jj-config.sh` to regenerate."
            );
        }
    }
}

/// Extract jj-lib version from Cargo.toml (e.g. "0.39" from `version = "0.39"`).
fn extract_jj_lib_version(cargo_toml: &str) -> Option<String> {
    // Look for `jj-lib = { version = "0.39"` or `jj-lib = "0.39"`
    let mut in_jj = false;
    for line in cargo_toml.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("jj-lib") {
            in_jj = true;
        }
        if in_jj {
            if let Some(pos) = trimmed.find("version") {
                // Extract the quoted version value
                let rest = &trimmed[pos..];
                if let Some(start) = rest.find('"') {
                    let after_quote = &rest[start + 1..];
                    if let Some(end) = after_quote.find('"') {
                        return Some(after_quote[..end].to_string());
                    }
                }
            }
            // Single-line dependency or end of inline table
            if trimmed.contains('}') || !trimmed.contains('{') {
                in_jj = false;
            }
        }
    }
    None
}

/// Extract `jj-version: X.Y.Z` from the vendored TOML header comments.
fn extract_vendored_version(toml: &str) -> Option<String> {
    for line in toml.lines() {
        if !line.starts_with('#') {
            break; // Past the header
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
