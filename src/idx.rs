//! Typed index newtypes and `IndexVec<I, T>` for compile-time safe indexing.
//!
//! Prevents accidentally using a `FileIdx` where an `EntryIdx` is expected.

use std::marker::PhantomData;

// ---------------------------------------------------------------------------
// Index newtypes
// ---------------------------------------------------------------------------

macro_rules! define_idx {
    ($(#[$meta:meta])* $vis:vis $name:ident) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        $vis struct $name(usize);

        impl $name {
            #[inline]
            pub const fn new(raw: usize) -> Self { Self(raw) }

            #[inline]
            pub const fn raw(self) -> usize { self.0 }
        }

        impl From<$name> for usize {
            #[inline]
            fn from(idx: $name) -> usize { idx.0 }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                self.0.fmt(f)
            }
        }
    };
}

define_idx!(
    /// Index into `App::entries` / `App::graph` / `App::unfolded`.
    pub EntryIdx
);
define_idx!(
    /// Index into the file list for a given commit (`App::file_cache[entry]`).
    pub FileIdx
);
define_idx!(
    /// Index into the diff lines for a given file (`App::diff_cache[(entry, file)]`).
    pub DiffLineIdx
);
define_idx!(
    /// Index into `GraphLines::extra` (link/pad/term lines between commits).
    pub GraphLineIdx
);
define_idx!(
    /// Index into `App::bookmark_entries`.
    pub BookmarkIdx
);
define_idx!(
    /// Index into `BookmarkDetails::conflict_targets` or `remote_targets`.
    pub BookmarkDetailIdx
);
define_idx!(
    /// Index into description continuation lines (skip(1) from full_description).
    pub DescriptionLineIdx
);
define_idx!(
    /// Index into `App::tag_entries`.
    pub TagIdx
);
define_idx!(
    /// Index into `TagDetails::remote_targets`.
    pub TagDetailIdx
);
define_idx!(
    /// Index into `App::op_log_entries`.
    pub OpLogIdx
);

// ---------------------------------------------------------------------------
// IndexVec<I, T> -- a Vec<T> indexed by a typed index I.
// ---------------------------------------------------------------------------

/// A `Vec<T>` that can only be indexed by a specific typed index `I`.
///
/// This prevents accidentally indexing with the wrong kind of index
/// (e.g., using a `FileIdx` to index into entries).
pub struct IndexVec<I, T> {
    vec: Vec<T>,
    _marker: PhantomData<I>,
}

impl<I, T> IndexVec<I, T>
where
    I: From<usize> + Copy,
    usize: From<I>,
{
    pub fn new() -> Self {
        Self {
            vec: Vec::new(),
            _marker: PhantomData,
        }
    }

    pub fn from_vec(vec: Vec<T>) -> Self {
        Self {
            vec,
            _marker: PhantomData,
        }
    }

    pub fn len(&self) -> usize {
        self.vec.len()
    }

    pub fn is_empty(&self) -> bool {
        self.vec.is_empty()
    }

    pub fn push(&mut self, value: T) -> I {
        let idx = I::from(self.vec.len());
        self.vec.push(value);
        idx
    }

    pub fn get(&self, idx: I) -> Option<&T> {
        self.vec.get(usize::from(idx))
    }

    pub fn get_mut(&mut self, idx: I) -> Option<&mut T> {
        self.vec.get_mut(usize::from(idx))
    }

    pub fn iter(&self) -> std::slice::Iter<'_, T> {
        self.vec.iter()
    }

    pub fn iter_mut(&mut self) -> std::slice::IterMut<'_, T> {
        self.vec.iter_mut()
    }

    /// Iterate with typed indices.
    pub fn iter_enumerated(&self) -> impl Iterator<Item = (I, &T)> {
        self.vec.iter().enumerate().map(|(i, v)| (I::from(i), v))
    }

    pub fn clear(&mut self) {
        self.vec.clear();
    }

    /// Get the underlying slice.
    pub fn as_slice(&self) -> &[T] {
        &self.vec
    }

    /// Consume the IndexVec and return the inner Vec.
    pub fn into_vec(self) -> Vec<T> {
        self.vec
    }
}

impl<I, T> Default for IndexVec<I, T>
where
    I: From<usize> + Copy,
    usize: From<I>,
{
    fn default() -> Self {
        Self::new()
    }
}

// We need From<usize> on our index types for IndexVec::push/iter_enumerated.
impl From<usize> for EntryIdx {
    fn from(v: usize) -> Self {
        Self(v)
    }
}
impl From<usize> for FileIdx {
    fn from(v: usize) -> Self {
        Self(v)
    }
}
impl From<usize> for DiffLineIdx {
    fn from(v: usize) -> Self {
        Self(v)
    }
}
impl From<usize> for GraphLineIdx {
    fn from(v: usize) -> Self {
        Self(v)
    }
}
impl From<usize> for BookmarkIdx {
    fn from(v: usize) -> Self {
        Self(v)
    }
}
impl From<usize> for BookmarkDetailIdx {
    fn from(v: usize) -> Self {
        Self(v)
    }
}
impl From<usize> for DescriptionLineIdx {
    fn from(v: usize) -> Self {
        Self(v)
    }
}
impl From<usize> for TagIdx {
    fn from(v: usize) -> Self {
        Self(v)
    }
}
impl From<usize> for TagDetailIdx {
    fn from(v: usize) -> Self {
        Self(v)
    }
}
impl From<usize> for OpLogIdx {
    fn from(v: usize) -> Self {
        Self(v)
    }
}

// Index and IndexMut impls for IndexVec.
impl<I, T> std::ops::Index<I> for IndexVec<I, T>
where
    usize: From<I>,
{
    type Output = T;
    fn index(&self, idx: I) -> &T {
        &self.vec[usize::from(idx)]
    }
}

impl<I, T> std::ops::IndexMut<I> for IndexVec<I, T>
where
    usize: From<I>,
{
    fn index_mut(&mut self, idx: I) -> &mut T {
        &mut self.vec[usize::from(idx)]
    }
}
