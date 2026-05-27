pub type Str = compact_str::CompactString;

/// A small vector optimized for the common 1-2 element case.
pub type SmallVec<T> = smallvec::SmallVec<[T; 2]>;

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
define_str_newtype!(pub CommitId);
define_str_newtype!(pub BookmarkName);
define_str_newtype!(pub TagName);
define_str_newtype!(pub RemoteName);
define_str_newtype!(pub RepoPath);
define_str_newtype!(pub OperationId);
define_str_newtype!(pub WorkspaceName);

/// A reference to a file associated with a change_id
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct FileRef {
    pub change_id: ChangeId,
    pub path: RepoPath,
}
