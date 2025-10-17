use borsh::{BorshDeserialize, BorshSerialize};
use num_derive::{FromPrimitive, ToPrimitive};

#[repr(u64)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, FromPrimitive, ToPrimitive)]
pub enum KeyspaceTag {
    /// Used for a context based storage which usually involves multi dimensional data i.e. maps,
    /// efficient vectors, etc.
    Context = 0,
    /// Used for a named key based storage which usually involves named keys.
    NamedKey = 1,
}

#[derive(BorshSerialize, BorshDeserialize, Clone, Debug, PartialEq, Eq)]
pub struct StateAddrInner {
    pub entity_addr: [u8; 32],
    field_addr: String,
}

impl StateAddrInner {
    pub fn new<T: Into<String>>(entity_addr: [u8; 32], field_addr: T) -> Self {
        let field_addr = field_addr.into();
        Self {
            entity_addr,
            field_addr,
        }
    }
}

#[derive(BorshSerialize, BorshDeserialize, Clone, Debug, PartialEq, Eq)]
pub struct CollectionAddrInner {
    pub entity_addr: [u8; 32],
    collection_type_tag: u8,
    collection_prefix: [u8; 8],
    tail: [u8; 32]
}

impl CollectionAddrInner {
    pub fn new(
        entity_addr: [u8; 32],
        collection_type_tag: u8,
        collection_prefix: [u8; 8],
        tail: [u8; 32]
    ) -> Self {
        Self {
            entity_addr,
            collection_type_tag,
            collection_prefix,
            tail,
        }
    }
}

#[derive(BorshSerialize, BorshDeserialize, Clone, Debug, PartialEq, Eq)]
pub enum ContextAddr {
    StateAddr(StateAddrInner),
    CollectionAddr(CollectionAddrInner)
}

impl From<StateAddrInner> for ContextAddr {
    fn from(value: StateAddrInner) -> Self {
        Self::StateAddr(value)
    }
}

impl From<CollectionAddrInner> for ContextAddr {
    fn from(value: CollectionAddrInner) -> Self {
        Self::CollectionAddr(value)
    }
}

#[repr(u64)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Keyspace<'a> {
    /// Stores contract's context data. Bytes can be any value as long as it uniquely identifies a
    /// value.
    Context(ContextAddr),
    /// Stores contract's named keys.
    NamedKey(&'a str),
}

impl Keyspace<'_> {
    #[must_use]
    pub fn as_tag(&self) -> KeyspaceTag {
        match self {
            Keyspace::Context(_) => KeyspaceTag::Context,
            Keyspace::NamedKey(_) => KeyspaceTag::NamedKey,
        }
    }

    #[must_use]
    pub fn as_u64(&self) -> u64 {
        self.as_tag() as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_as_tag_context() {
        let data = StateAddrInner::new([0; 32], "some-field");
        let keyspace = Keyspace::Context(data.into());
        assert_eq!(keyspace.as_tag(), KeyspaceTag::Context);
    }

    #[test]
    fn test_as_tag_named_key() {
        let name = "my_key";
        let keyspace = Keyspace::NamedKey(name);
        assert_eq!(keyspace.as_tag(), KeyspaceTag::NamedKey);
    }

    #[test]
    fn test_as_u64_context() {
        let data = StateAddrInner::new([0; 32], "some-field");
        let keyspace = Keyspace::Context(data.into());
        assert_eq!(keyspace.as_u64(), 0);
    }

    #[test]
    fn test_as_u64_named_key() {
        let name = "my_key";
        let keyspace = Keyspace::NamedKey(name);
        assert_eq!(keyspace.as_u64(), 1);
    }
}
