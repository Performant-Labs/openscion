use std::fmt;

const BLOB_DOMAIN: &[u8] = b"scion\0blob\0v1\0";
const CHANGESET_DOMAIN: &[u8] = b"scion\0changeset\0v1\0";
const TREE_DOMAIN: &[u8] = b"scion\0tree\0v1\0";

fn domain_hash(domain: &[u8], bytes: &[u8]) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(domain);
    hasher.update(bytes);
    *hasher.finalize().as_bytes()
}

macro_rules! object_id {
    ($(#[$doc:meta])* $name:ident, $domain:expr) => {
        $(#[$doc])*
        #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name([u8; 32]);

        impl $name {
            /// Wraps raw ID bytes. Does not check that any object has this ID.
            #[must_use]
            pub const fn from_bytes(bytes: [u8; 32]) -> Self {
                Self(bytes)
            }

            /// The raw ID bytes.
            #[must_use]
            pub const fn as_bytes(&self) -> &[u8; 32] {
                &self.0
            }

            /// Hashes `bytes` under this ID kind's domain prefix.
            pub(crate) fn hash(bytes: &[u8]) -> Self {
                Self(domain_hash($domain, bytes))
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&blake3::Hash::from_bytes(self.0).to_hex())
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, concat!(stringify!($name), "({})"), self)
            }
        }
    };
}

object_id!(
    /// Content address of a file's raw bytes.
    BlobId,
    BLOB_DOMAIN
);
object_id!(
    /// Content address of an encoded [`Changeset`](crate::Changeset).
    ChangesetId,
    CHANGESET_DOMAIN
);
object_id!(
    /// Content address of an encoded [`Tree`](crate::Tree).
    TreeId,
    TREE_DOMAIN
);

impl BlobId {
    /// The ID of a file's raw, uncompressed content.
    #[must_use]
    pub fn of(raw: &[u8]) -> Self {
        Self::hash(raw)
    }
}
