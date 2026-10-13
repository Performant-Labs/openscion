//! Every input the decoders accept must be canonical: decoding then
//! encoding gives back exactly the input bytes.

#![no_main]

use libfuzzer_sys::fuzz_target;
use scion_core::{Changeset, Tree};

fuzz_target!(|data: &[u8]| {
    if let Ok(cs) = Changeset::decode(data) {
        assert_eq!(cs.encode(), data);
    }
    if let Ok(tree) = Tree::decode(data) {
        assert_eq!(tree.encode(), data);
    }
});
