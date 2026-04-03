pub type Str = compact_str::CompactString;

/// A small vector optimized for the common 1-2 element case.
pub type SmallVec<T> = smallvec::SmallVec<[T; 2]>;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct ChangeId(Str);

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct CommitId(Str);

impl std::fmt::Display for ChangeId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl ChangeId {
    pub fn new(s: impl Into<Str>) -> Self {
        Self(s.into())
    }

    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl std::fmt::Display for CommitId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl CommitId {
    pub fn new(s: impl Into<Str>) -> Self {
        Self(s.into())
    }

    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl PartialEq<String> for CommitId {
    fn eq(&self, other: &String) -> bool {
        self.as_str().eq(other.as_str())
    }
}

impl PartialEq<str> for CommitId {
    fn eq(&self, other: &str) -> bool {
        self.as_str().eq(other)
    }
}

impl PartialEq<String> for ChangeId {
    fn eq(&self, other: &String) -> bool {
        self.as_str().eq(other.as_str())
    }
}

impl PartialEq<str> for ChangeId {
    fn eq(&self, other: &str) -> bool {
        self.as_str().eq(other)
    }
}

/// A reference to a file associated with a change_id
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct FileRef {
    pub change_id: ChangeId,
    pub path: String,
}
