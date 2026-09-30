pub type Str = compact_str::CompactString;

/// A small vector optimized for the common 1-2 element case.
pub type SmallVec<T> = smallvec::SmallVec<[T; 2]>;

/// A non-empty [`SmallVec`]. Guarantees at least one element at the type level.
pub type SmallVec1<T> = vec1::smallvec_v1::SmallVec1<[T; 2]>;

/// Define a newtype wrapper around `Str` (CompactString) with standard impls.
macro_rules! define_str_newtype {
    ($(#[$meta:meta])* $vis:vis $name:ident) => {
        $(#[$meta])*
        #[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        #[repr(transparent)]
        $vis struct $name(Str);

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "{}", self.0)
            }
        }

        impl $name {
            pub fn new(s: impl Into<Str>) -> Self { Self(s.into()) }
            pub fn as_str(&self) -> &str { self.0.as_str() }
        }

        impl PartialEq<String> for $name {
            fn eq(&self, other: &String) -> bool {
                self.as_str() == other.as_str()
            }
        }

        impl PartialEq<str> for $name {
            fn eq(&self, other: &str) -> bool {
                self.as_str() == other
            }
        }
    };
}

define_str_newtype!(pub ChangeId);
define_str_newtype!(
    /// A revision to hand `jj` on the command line.
    ///
    /// Usually the shortest unique change ID prefix, but jj accepts any
    /// revision here and kojutsu uses several forms: a `<change ID>/<offset>`
    /// when the change is divergent or hidden, and a commit ID prefix where a
    /// change ID would be ambiguous or meaningless (evolog rows, for
    /// instance). Distinct from [`ChangeId`], which identifies a change and is
    /// what internal state is keyed by.
    pub RevisionArg
);
define_str_newtype!(pub CommitId);
define_str_newtype!(pub BookmarkName);
define_str_newtype!(pub TagName);
define_str_newtype!(pub RemoteName);
define_str_newtype!(pub RepoPath);
define_str_newtype!(pub OperationId);
define_str_newtype!(pub WorkspaceName);

/// A file in one commit.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct FileRef {
    pub commit_id: CommitId,
    pub path: RepoPath,
}

/// A selected commit. Its commit ID is its identity; its change ID, without
/// a divergence offset, is how jj is asked for it while the DAG hasn't
/// loaded it again, since the commit ID may name what a rewrite replaced.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct CommitRef {
    pub commit_id: CommitId,
    pub change_id: ChangeId,
}
