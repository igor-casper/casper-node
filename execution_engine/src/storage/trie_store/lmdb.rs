//! An LMDB-backed trie store.
//!
//! # Usage
//!
//! ```
//! use casper_execution_engine::storage::store::Store;
//! use casper_execution_engine::storage::transaction_source::{Transaction, TransactionSource};
//! use casper_execution_engine::storage::transaction_source::lmdb::LmdbEnvironment;
//! use casper_execution_engine::storage::trie::{Pointer, PointerBlock, Trie};
//! use casper_execution_engine::storage::trie_store::lmdb::LmdbTrieStore;
//! use casper_hashing::Digest;
//! use casper_types::bytesrepr::{ToBytes, Bytes};
//! use lmdb::DatabaseFlags;
//! use tempfile::tempdir;
//!
//! // Create some leaves
//! let leaf_1 = Trie::Leaf { key: Bytes::from(vec![0u8, 0, 0]), value: Bytes::from(b"val_1".to_vec()) };
//! let leaf_2 = Trie::Leaf { key: Bytes::from(vec![1u8, 0, 0]), value: Bytes::from(b"val_2".to_vec()) };
//!
//! // Get their hashes
//! let leaf_1_hash = Digest::hash(&leaf_1.to_bytes().unwrap());
//! let leaf_2_hash = Digest::hash(&leaf_2.to_bytes().unwrap());
//!
//! // Create a node
//! let node: Trie<Bytes, Bytes> = {
//!     let mut pointer_block = PointerBlock::new();
//!     pointer_block[0] = Some(Pointer::LeafPointer(leaf_1_hash));
//!     pointer_block[1] = Some(Pointer::LeafPointer(leaf_2_hash));
//!     let pointer_block = Box::new(pointer_block);
//!     Trie::Node { pointer_block }
//! };
//!
//! // Get its hash
//! let node_hash = Digest::hash(&node.to_bytes().unwrap());
//!
//! // Create the environment and the store. For both the in-memory and
//! // LMDB-backed implementations, the environment is the source of
//! // transactions.
//! let tmp_dir = tempdir().unwrap();
//! let map_size = 4096 * 2560;  // map size should be a multiple of OS page size
//! let max_readers = 512;
//! let env = LmdbEnvironment::new(&tmp_dir.path().to_path_buf(), map_size, max_readers, true).unwrap();
//! let store = LmdbTrieStore::new(&env, None, DatabaseFlags::empty()).unwrap();
//!
//! // First let's create a read-write transaction, persist the values, but
//! // forget to commit the transaction.
//! {
//!     // Create a read-write transaction
//!     let mut txn = env.create_read_write_txn().unwrap();
//!
//!     // Put the values in the store
//!     store.put(&mut txn, &leaf_1_hash, &leaf_1).unwrap();
//!     store.put(&mut txn, &leaf_2_hash, &leaf_2).unwrap();
//!     store.put(&mut txn, &node_hash, &node).unwrap();
//!
//!     // Here we forget to commit the transaction before it goes out of scope
//! }
//!
//! // Now let's check to see if the values were stored
//! {
//!     // Create a read transaction
//!     let txn = env.create_read_txn().unwrap();
//!
//!     // Observe that nothing has been persisted to the store
//!     for hash in vec![&leaf_1_hash, &leaf_2_hash, &node_hash].iter() {
//!         // We need to use a type annotation here to help the compiler choose
//!         // a suitable FromBytes instance
//!         let maybe_trie: Option<Trie<Bytes, Bytes>> = store.get(&txn, hash).unwrap();
//!         assert!(maybe_trie.is_none());
//!     }
//!
//!     // Commit the read transaction.  Not strictly necessary, but better to be hygienic.
//!     txn.commit().unwrap();
//! }
//!
//! // Now let's try that again, remembering to commit the transaction this time
//! {
//!     // Create a read-write transaction
//!     let mut txn = env.create_read_write_txn().unwrap();
//!
//!     // Put the values in the store
//!     store.put(&mut txn, &leaf_1_hash, &leaf_1).unwrap();
//!     store.put(&mut txn, &leaf_2_hash, &leaf_2).unwrap();
//!     store.put(&mut txn, &node_hash, &node).unwrap();
//!
//!     // Commit the transaction.
//!     txn.commit().unwrap();
//! }
//!
//! // Now let's check to see if the values were stored again
//! {
//!     // Create a read transaction
//!     let txn = env.create_read_txn().unwrap();
//!
//!     // Get the values in the store
//!     assert_eq!(Some(leaf_1), store.get(&txn, &leaf_1_hash).unwrap());
//!     assert_eq!(Some(leaf_2), store.get(&txn, &leaf_2_hash).unwrap());
//!     assert_eq!(Some(node), store.get(&txn, &node_hash).unwrap());
//!
//!     // Commit the read transaction.
//!     txn.commit().unwrap();
//! }
//!
//! tmp_dir.close().unwrap();
//! ```
use std::{
    borrow::Cow,
    collections::{hash_map::Entry, HashMap},
    convert::TryFrom,
    ops::Deref,
    sync::{Arc, Mutex},
};

use casper_types::{
    bytesrepr::{self, Bytes, ToBytes},
    Key, StoredValue,
};
use lmdb::{Database, DatabaseFlags, Transaction};

use casper_hashing::Digest;

use crate::storage::{
    error,
    global_state::CommitError,
    store::Store,
    transaction_source::{lmdb::LmdbEnvironment, Readable, TransactionSource, Writable},
    trie::{DescendantsIterator, Trie},
    trie_store::{self, TrieStore},
};

/// An LMDB-backed trie store.
///
/// Wraps [`lmdb::Database`].
#[derive(Debug, Clone)]
pub struct LmdbTrieStore {
    db: Database,
}

impl LmdbTrieStore {
    /// Constructor for new `LmdbTrieStore`.
    pub fn new(
        env: &LmdbEnvironment,
        maybe_name: Option<&str>,
        flags: DatabaseFlags,
    ) -> Result<Self, error::Error> {
        let name = Self::name(maybe_name);
        let db = env.env().create_db(Some(&name), flags)?;
        Ok(LmdbTrieStore { db })
    }

    /// Constructor for `LmdbTrieStore` which opens an existing lmdb store file.
    pub fn open(env: &LmdbEnvironment, maybe_name: Option<&str>) -> Result<Self, error::Error> {
        let name = Self::name(maybe_name);
        let db = env.env().open_db(Some(&name))?;
        Ok(LmdbTrieStore { db })
    }

    fn name(maybe_name: Option<&str>) -> String {
        maybe_name
            .map(|name| format!("{}-{}", trie_store::NAME, name))
            .unwrap_or_else(|| String::from(trie_store::NAME))
    }

    /// Get a handle to the underlying database.
    pub fn get_db(&self) -> Database {
        self.db
    }
}

impl<K, V> Store<Digest, Trie<K, V>> for LmdbTrieStore {
    type Error = error::Error;

    type Handle = Database;

    fn handle(&self) -> Self::Handle {
        self.db
    }
}

impl<K, V> TrieStore<K, V> for LmdbTrieStore {}

/// Cache used by the scratch trie.  The keys represent the hash of the trie being cached.  The
/// values represent:  1) A boolean, where `false` means the trie was _not_ written and `true` means
/// it was 2) A deserialized trie
pub(crate) type Cache = Arc<Mutex<HashMap<Digest, (bool, Bytes)>>>;

/// Cached version of the trie store.
#[derive(Clone)]
pub(crate) struct ScratchTrieStore {
    pub(crate) cache: Cache,
    pub(crate) store: Arc<LmdbTrieStore>,
    pub(crate) env: Arc<LmdbEnvironment>,
}

fn trie_bytes_iter_children(trie_bytes: &[u8]) -> Result<DescendantsIterator, bytesrepr::Error> {
    match trie_bytes.first() {
        Some(tag) if tag == &0 => Ok(DescendantsIterator::ZeroOrOne(None)),
        Some(_tag) => {
            // We can deserialize trie as we know this is either a node or an extension
            let trie: Trie<Key, StoredValue> = bytesrepr::deserialize_from_slice(trie_bytes)?;
            Ok(trie.iter_children())
        }
        None => Err(bytesrepr::Error::Formatting),
    }
}

impl ScratchTrieStore {
    /// Creates a new ScratchTrieStore.
    pub fn new(store: Arc<LmdbTrieStore>, env: Arc<LmdbEnvironment>) -> Self {
        Self {
            store,
            env,
            cache: Default::default(),
        }
    }

    /// Writes only tries which are both under the given `state_root` and dirty to the underlying db
    /// while maintaining the invariant that children must be written before parent nodes.
    pub fn write_root_to_db(self, state_root: Digest) -> Result<(), error::Error> {
        let env = self.env;
        let store = self.store;
        let cache = &mut *self.cache.lock().map_err(|_| error::Error::Poison)?;

        let (is_root_dirty, root_trie) = cache
            .get(&state_root)
            .ok_or(CommitError::TrieNotFoundInCache(state_root))?;

        // Early exit if there is no work to do.
        if !is_root_dirty {
            return Ok(());
        }

        let mut txn = env.create_read_write_txn()?;
        let mut tries_to_visit =
            vec![(state_root, root_trie, trie_bytes_iter_children(root_trie)?)];

        while let Some((digest, current_trie, mut descendants_iterator)) = tries_to_visit.pop() {
            if let Some(descendant) = descendants_iterator.next() {
                tries_to_visit.push((digest, current_trie, descendants_iterator));
                // Only if a node is marked as dirty in the cache do we want to visit it's
                // children.
                if let Some((true, child_trie)) = cache.get(&descendant) {
                    let children = trie_bytes_iter_children(child_trie)?;
                    tries_to_visit.push((descendant, child_trie, children));
                }
            } else {
                Store::<Digest, Trie<Key, StoredValue>>::put_raw(
                    store.deref(),
                    &mut txn,
                    &digest.value(),
                    Cow::Borrowed(current_trie),
                )?;
            }
        }

        txn.commit()?;
        Ok(())
    }
}

impl Store<Digest, Trie<Key, StoredValue>> for ScratchTrieStore {
    type Error = error::Error;

    type Handle = ScratchTrieStore;

    fn handle(&self) -> Self::Handle {
        self.clone()
    }

    fn get_raw<T>(&self, txn: &T, key: &Digest) -> Result<Option<Bytes>, Self::Error>
    where
        T: Readable<Handle = Self::Handle>,
        Digest: AsRef<[u8]>,
        Self::Error: From<T::Error>,
    {
        let mut store = self.cache.lock().map_err(|_| error::Error::Poison)?;

        let maybe_trie = store.get(key);

        match maybe_trie {
            Some((_, trie_bytes)) => Ok(Some(trie_bytes.clone())),
            None => {
                let handle = self.handle();
                match txn.read(handle, key.as_ref())? {
                    Some(trie_bytes) => {
                        match store.entry(*key) {
                            Entry::Occupied(_) => {}
                            Entry::Vacant(v) => {
                                v.insert((false, trie_bytes.clone()));
                            }
                        }
                        Ok(Some(trie_bytes))
                    }
                    None => Ok(None),
                }
            }
        }
    }

    fn put_raw<'a, T>(
        &self,
        _txn: &mut T,
        key_bytes: &[u8],
        value_bytes: Cow<'a, [u8]>,
    ) -> Result<(), Self::Error>
    where
        T: Writable<Handle = Self::Handle>,
        Self::Error: From<T::Error>,
    {
        debug_assert_eq!(
            key_bytes.len(),
            32,
            "Should only use Digest bytes in this impl"
        );
        let key = Digest::try_from(key_bytes).unwrap(); // SAFETY: we're inside impl Store<Digest, ...>
        self.cache
            .lock()
            .map_err(|_| error::Error::Poison)?
            .insert(key, (true, Bytes::from(value_bytes.into_owned())));
        Ok(())
    }

    fn put<T>(
        &self,
        txn: &mut T,
        key: &Digest,
        value: &Trie<Key, StoredValue>,
    ) -> Result<(), Self::Error>
    where
        T: Writable<Handle = Self::Handle>,
        Trie<Key, StoredValue>: ToBytes,
        Self::Error: From<T::Error>,
    {
        self.put_raw(txn, key.as_ref(), Cow::Owned(value.to_bytes()?))
    }
}

impl TrieStore<Key, StoredValue> for ScratchTrieStore {}
