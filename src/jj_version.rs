//! Which `jj` binary kojutsu is driving, and what it can be asked to do.
//!
//! kojutsu reads the repo through the jj-lib it links, but every change goes
//! through the `jj` on `PATH`, which may be older or newer than that. The
//! command line it builds is plain jj syntax, so a flag the installed jj
//! predates fails with clap's "unexpected argument". [`JjFeature`] dates the
//! arguments kojutsu emits that are newer than [`FLOOR`]; the args builder
//! tags each one where it spells it, and a command is refused before it runs
//! when the installed jj predates anything it carries.

use std::fmt;

/// A jj release, as `jj --version` reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct JjVersion {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl JjVersion {
    pub const fn new(major: u32, minor: u32, patch: u32) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }

    /// Read a version out of `jj --version` output (`jj 0.45.1`), or a bare
    /// one (`0.45.1`). A development build's suffix (`0.47.0-1a2b3c`) is
    /// dropped: it reports the release it is heading for.
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        let text = text.strip_prefix("jj ").unwrap_or(text);
        let end = text
            .find(|c: char| !c.is_ascii_digit() && c != '.')
            .unwrap_or(text.len());
        let mut parts = text[..end].split('.').map(str::parse::<u32>);
        let major = parts.next()?.ok()?;
        let minor = parts.next()?.ok()?;
        let patch = match parts.next() {
            Some(patch) => patch.ok()?,
            None => 0,
        };
        Some(Self::new(major, minor, patch))
    }

    /// The jj-lib kojutsu reads the repo with, as Cargo.lock pins it.
    pub fn linked() -> Self {
        Self::parse(env!("KOJUTSU_JJ_LIB_VERSION")).expect("build.rs records a jj-lib version")
    }

    /// Whether the two are the same release series, which is all that
    /// decides what a binary accepts or how it writes the repo.
    fn same_release(self, other: Self) -> bool {
        (self.major, self.minor) == (other.major, other.minor)
    }
}

impl fmt::Display for JjVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// The oldest jj kojutsu supports: the first to take `--onto`, which rebase,
/// squash, split, revert and duplicate are all built on. Everything kojutsu
/// emits without a [`JjFeature`] works from here.
pub const FLOOR: JjVersion = JjVersion::new(0, 36, 0);

/// A piece of jj's command line newer than [`FLOOR`], dated by the release
/// that first accepted it in the spelling kojutsu emits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, strum::EnumIter)]
pub enum JjFeature {
    BookmarkAdvance,
    Run,
    RunPassthrough,
    RunIgnoreChanges,
    RunIgnoreErrors,
    PushAllowConflicts,
    /// `jj absorb --interactive`, which line selections go through.
    AbsorbLines,
    /// `jj tag track` and `jj tag untrack`.
    TagTracking,
    Converge,
    /// `jj undo`/`jj redo --allow-cross-workspace`.
    UndoCrossWorkspace,
    /// `jj workspace add --colocate` and `--no-colocate`.
    WorkspaceColocation,
    WorkspaceRemove,
    /// `jj git colocation` run from a workspace other than the main one.
    ColocationInWorkspaces,
    /// `jj git push` with more than one `--remote`.
    PushToRemotes,
}

impl JjFeature {
    /// The release that introduced it. The only place a version is written.
    pub const fn since(self) -> JjVersion {
        match self {
            Self::BookmarkAdvance => JjVersion::new(0, 39, 0),
            Self::Run => JjVersion::new(0, 43, 0),
            Self::RunPassthrough
            | Self::RunIgnoreChanges
            | Self::RunIgnoreErrors
            | Self::PushAllowConflicts
            | Self::AbsorbLines
            | Self::TagTracking => JjVersion::new(0, 44, 0),
            Self::Converge => JjVersion::new(0, 45, 0),
            Self::UndoCrossWorkspace
            | Self::WorkspaceColocation
            | Self::WorkspaceRemove
            | Self::ColocationInWorkspaces
            | Self::PushToRemotes => JjVersion::new(0, 46, 0),
        }
    }

    /// What it is, as a message names it.
    pub const fn name(self) -> &'static str {
        match self {
            Self::BookmarkAdvance => "jj bookmark advance",
            Self::Run => "jj run",
            Self::RunPassthrough => "jj run --passthrough",
            Self::RunIgnoreChanges => "jj run --ignore-changes",
            Self::RunIgnoreErrors => "jj run --ignore-errors",
            Self::PushAllowConflicts => "jj git push --allow-conflicts",
            Self::AbsorbLines => "absorbing a line selection",
            Self::TagTracking => "tracking tags",
            Self::Converge => "jj converge",
            Self::UndoCrossWorkspace => "undoing another workspace's operation",
            Self::WorkspaceColocation => "choosing whether a new workspace is colocated",
            Self::WorkspaceRemove => "jj workspace remove",
            Self::ColocationInWorkspaces => "jj git colocation outside the main workspace",
            Self::PushToRemotes => "pushing to several remotes at once",
        }
    }
}

/// The `jj` on `PATH`, as far as kojutsu could tell. Unknown when
/// `jj --version` could not be run or read; nothing is held back then, since
/// a binary kojutsu can't date is as likely a newer one as anything.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct InstalledJj(Option<JjVersion>);

impl InstalledJj {
    pub const fn known(version: JjVersion) -> Self {
        Self(Some(version))
    }

    /// Ask `jj --version`.
    pub fn probe() -> Self {
        let output = std::process::Command::new("jj")
            .arg("--version")
            .stdin(std::process::Stdio::null())
            .output();
        match output {
            Ok(output) if output.status.success() => {
                let text = String::from_utf8_lossy(&output.stdout);
                let version = JjVersion::parse(&text);
                if version.is_none() {
                    tracing::warn!(output = %text.trim(), "unrecognised `jj --version` output");
                }
                Self(version)
            }
            Ok(output) => {
                tracing::warn!(status = %output.status, "`jj --version` failed");
                Self(None)
            }
            Err(e) => {
                tracing::warn!(error = %e, "could not run `jj --version`");
                Self(None)
            }
        }
    }

    pub fn version(self) -> Option<JjVersion> {
        self.0
    }

    pub fn supports(self, feature: JjFeature) -> bool {
        self.0.is_none_or(|version| version >= feature.since())
    }

    /// Why `feature` can't be used, for a message.
    pub fn refusal(self, feature: JjFeature) -> Option<String> {
        let version = self.0.filter(|_| !self.supports(feature))?;
        Some(format!(
            "{} needs jj {} or newer; the installed jj is {version}",
            feature.name(),
            feature.since()
        ))
    }

    /// What is worth telling the user about this binary at startup: one
    /// older than kojutsu supports, or one newer than the jj-lib it reads
    /// the repo with, which may write what that library misreads.
    pub fn concerns(self) -> Vec<String> {
        let Some(version) = self.0 else {
            return Vec::new();
        };
        let linked = JjVersion::linked();
        let mut concerns = Vec::new();
        if version < FLOOR {
            concerns.push(format!(
                "jj {version} is older than {FLOOR}, the oldest kojutsu supports: \
                 commands may fail"
            ));
        }
        if version > linked && !version.same_release(linked) {
            concerns.push(format!(
                "jj {version} is newer than the jj-lib {linked} kojutsu reads the repo with: \
                 if it changed how the repo is stored, kojutsu may misread it"
            ));
        }
        concerns
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_what_jj_version_prints() {
        assert_eq!(
            JjVersion::parse("jj 0.45.1\n"),
            Some(JjVersion::new(0, 45, 1))
        );
        assert_eq!(JjVersion::parse("0.46.0"), Some(JjVersion::new(0, 46, 0)));
        // A development build names the release it is heading for.
        assert_eq!(
            JjVersion::parse("jj 0.47.0-1a2b3c4d5e"),
            Some(JjVersion::new(0, 47, 0))
        );
        assert_eq!(JjVersion::parse("jj nightly"), None);
        assert_eq!(JjVersion::parse(""), None);
    }

    #[test]
    fn orders_by_release() {
        assert!(JjVersion::new(0, 9, 0) < JjVersion::new(0, 10, 0));
        assert!(JjVersion::new(0, 45, 1) > JjVersion::new(0, 45, 0));
        assert!(JjVersion::new(1, 0, 0) > JjVersion::new(0, 99, 9));
    }

    #[test]
    fn the_linked_jj_lib_is_recorded() {
        assert!(JjVersion::linked() >= FLOOR);
    }

    #[test]
    fn every_feature_is_newer_than_the_floor() {
        use strum::IntoEnumIterator;
        for feature in JjFeature::iter() {
            assert!(
                feature.since() > FLOOR,
                "{feature:?} belongs under the floor"
            );
        }
    }

    #[test]
    fn an_unknown_binary_is_held_to_nothing() {
        let unknown = InstalledJj::default();
        assert!(unknown.supports(JjFeature::Converge));
        assert_eq!(unknown.refusal(JjFeature::Converge), None);
        assert!(unknown.concerns().is_empty());
    }

    #[test]
    fn a_feature_is_refused_before_its_release() {
        let jj = InstalledJj::known(JjVersion::new(0, 44, 2));
        assert!(jj.supports(JjFeature::RunPassthrough));
        assert!(!jj.supports(JjFeature::Converge));
        assert_eq!(
            jj.refusal(JjFeature::Converge).as_deref(),
            Some("jj converge needs jj 0.45.0 or newer; the installed jj is 0.44.2")
        );
    }

    #[test]
    fn concerns_cover_a_binary_too_old_or_newer_than_the_library() {
        let linked = JjVersion::linked();
        assert!(InstalledJj::known(linked).concerns().is_empty());
        // A patch release of the same series writes the same repo.
        let patched = JjVersion::new(linked.major, linked.minor, linked.patch + 1);
        assert!(InstalledJj::known(patched).concerns().is_empty());
        let next = JjVersion::new(linked.major, linked.minor + 1, 0);
        assert_eq!(InstalledJj::known(next).concerns().len(), 1);
        let ancient = JjVersion::new(0, 30, 0);
        assert_eq!(InstalledJj::known(ancient).concerns().len(), 1);
    }
}
