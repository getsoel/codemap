/// Content hashing with blake3.
pub fn hash_bytes(data: &[u8]) -> String {
    blake3::hash(data).to_hex().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic() {
        let h1 = hash_bytes(b"hello world");
        let h2 = hash_bytes(b"hello world");
        assert_eq!(h1, h2);
    }

    #[test]
    fn different_inputs_different_hashes() {
        let h1 = hash_bytes(b"hello");
        let h2 = hash_bytes(b"world");
        assert_ne!(h1, h2);
    }

    #[test]
    fn empty_input() {
        let h = hash_bytes(b"");
        assert!(!h.is_empty());
    }

    #[test]
    fn hash_is_64_hex_chars() {
        let h = hash_bytes(b"test");
        assert_eq!(h.len(), 64);
        assert!(h.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
