//! A vector made of shared chunks.
//!
//! The index is one writer (the background scan) and many readers (requests working on a snapshot). The scan keeps
//! appending rows and now and then changes an old one (a span's end arrives long after its start). With a plain `Vec`
//! every published snapshot would be a full copy. Here a snapshot is a clone of the chunk list: it shares every chunk
//! with the writer, and the writer copies a chunk only when it changes one that a snapshot still holds
//! (`Arc::make_mut`). Memory stays close to one copy of the data, and publishing costs a few pointer copies.

use std::sync::Arc;

const SHIFT: usize = 12;
/// Rows per chunk.
pub const CHUNK: usize = 1 << SHIFT;
const MASK: usize = CHUNK - 1;

#[derive(Debug)]
pub struct ChunkVec<T> {
    chunks: Vec<Arc<Vec<T>>>,
    len: usize,
}

impl<T> Default for ChunkVec<T> {
    fn default() -> Self {
        ChunkVec {
            chunks: Vec::new(),
            len: 0,
        }
    }
}

impl<T> Clone for ChunkVec<T> {
    /// A snapshot: shares every chunk.
    fn clone(&self) -> Self {
        ChunkVec {
            chunks: self.chunks.clone(),
            len: self.len,
        }
    }
}

impl<T: Clone> ChunkVec<T> {
    pub fn push(&mut self, v: T) {
        if self.len & MASK == 0 {
            self.chunks.push(Arc::new(Vec::with_capacity(CHUNK)));
        }
        if let Some(last) = self.chunks.last_mut() {
            Arc::make_mut(last).push(v);
            self.len += 1;
        }
    }

    /// A row to change. Copies its chunk first when a snapshot shares it.
    pub fn get_mut(&mut self, i: usize) -> Option<&mut T> {
        if i >= self.len {
            return None;
        }
        Arc::make_mut(self.chunks.get_mut(i >> SHIFT)?).get_mut(i & MASK)
    }
}

impl<T> ChunkVec<T> {
    #[must_use]
    pub fn len(&self) -> usize {
        self.len
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    #[must_use]
    pub fn get(&self, i: usize) -> Option<&T> {
        if i >= self.len {
            return None;
        }
        self.chunks.get(i >> SHIFT)?.get(i & MASK)
    }

    pub fn iter(&self) -> impl Iterator<Item = &T> {
        self.chunks.iter().flat_map(|c| c.iter())
    }
}

impl<T: Clone> FromIterator<T> for ChunkVec<T> {
    fn from_iter<I: IntoIterator<Item = T>>(iter: I) -> Self {
        let mut v = ChunkVec::default();
        for x in iter {
            v.push(x);
        }
        v
    }
}

#[cfg(test)]
#[allow(clippy::cast_possible_truncation)]
mod tests {
    use super::*;

    #[test]
    fn a_snapshot_keeps_what_it_saw() {
        let mut v: ChunkVec<u32> = ChunkVec::default();
        for i in 0..(CHUNK as u32 * 2 + 10) {
            v.push(i);
        }
        let snap = v.clone();
        *v.get_mut(5).unwrap() = 999;
        *v.get_mut(CHUNK + 1).unwrap() = 998;
        v.push(7);
        assert_eq!(snap.get(5), Some(&5));
        assert_eq!(snap.get(CHUNK + 1), Some(&(CHUNK as u32 + 1)));
        assert_eq!(snap.len(), CHUNK * 2 + 10);
        assert_eq!(snap.get(CHUNK * 2 + 10), None);
        assert_eq!(v.get(5), Some(&999));
        assert_eq!(v.get(CHUNK * 2 + 10), Some(&7));
        assert_eq!(v.iter().count(), CHUNK * 2 + 11);
        assert!(v.get_mut(CHUNK * 2 + 11).is_none());
        // the untouched last chunk is still shared, the changed ones are not
        assert!(!Arc::ptr_eq(&v.chunks[0], &snap.chunks[0]));
        assert!(!Arc::ptr_eq(&v.chunks[1], &snap.chunks[1]));
        let again = v.clone();
        assert!(Arc::ptr_eq(&v.chunks[2], &again.chunks[2]));
    }
}
