use anyhow::{bail, Result};
use std::fmt;

/// Crockford Base32 alphabet omitting ambiguous characters (I, L, O, U)
const BASE32_ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
pub const CODE_CHAR_LEN: usize = 12;

#[derive(Clone, PartialEq, Eq, Hash)]
pub struct PairingCode {
    raw: String, // 12 uppercase Base32 characters
}

impl PairingCode {
    /// Generate a cryptographically secure random 12-character pairing code
    /// 12 characters * 5 bits = 60 bits of entropy (> 1.15 * 10^18 combinations)
    pub fn generate() -> Result<Self> {
        let mut random_bytes = [0u8; CODE_CHAR_LEN];
        getrandom::getrandom(&mut random_bytes)
            .map_err(|e| anyhow::anyhow!("Failed to acquire secure entropy: {}", e))?;

        let mut raw = String::with_capacity(CODE_CHAR_LEN);
        for byte in random_bytes {
            let idx = (byte as usize) % BASE32_ALPHABET.len();
            raw.push(BASE32_ALPHABET[idx] as char);
        }

        Ok(Self { raw })
    }

    /// Parse and normalize user input (tolerates hyphens, whitespace, lowercase, and common typos)
    pub fn parse(input: &str) -> Result<Self> {
        let mut normalized = String::with_capacity(CODE_CHAR_LEN);
        for ch in input.chars() {
            match ch.to_ascii_uppercase() {
                '-' | ' ' | '\t' | '\r' | '\n' => continue,
                // Helpful normalization of easily confused characters
                'O' => normalized.push('0'),
                'I' | 'L' => normalized.push('1'),
                c if c.is_ascii() && BASE32_ALPHABET.contains(&(c as u8)) => normalized.push(c),
                other => bail!("Carácter no válido en el código: '{}'", other),
            }
        }

        if normalized.len() != CODE_CHAR_LEN {
            bail!(
                "El código de conexión debe tener 12 caracteres (recibidos {})",
                normalized.len()
            );
        }

        Ok(Self { raw: normalized })
    }

    /// Return the raw normalized 12-character string
    pub fn raw(&self) -> &str {
        &self.raw
    }

    /// Return the user-facing formatted string (e.g., "KD7M-9Q2P-W8RX")
    pub fn formatted(&self) -> String {
        format!(
            "{}-{}-{}",
            &self.raw[0..4],
            &self.raw[4..8],
            &self.raw[8..12]
        )
    }

    /// Derive root authentication key from code
    pub fn derive_auth_key(&self) -> [u8; 32] {
        blake3::derive_key("heavydrop-v3-code-auth-key", self.raw.as_bytes())
    }

    /// Blind ticket for local LAN beacon (zero disclosure of pairing code)
    pub fn lan_blind_ticket(&self) -> [u8; 32] {
        let root = self.derive_auth_key();
        *blake3::keyed_hash(&root, b"lan-blind-discovery-ticket").as_bytes()
    }

    /// Blind lookup hash for signaling server registration and resolution
    pub fn signaling_lookup_hash(&self) -> String {
        let root = self.derive_auth_key();
        let hash = blake3::keyed_hash(&root, b"signaling-rendezvous-lookup");
        hash.to_hex().to_string()
    }

    /// Compute mutual authentication proof for sender
    pub fn compute_sender_proof(&self, session_salt: &[u8; 32], sender_pub: &[u8; 32]) -> [u8; 32] {
        let mut data = Vec::with_capacity(32 + 32 + 32);
        data.extend_from_slice(&self.derive_auth_key());
        data.extend_from_slice(session_salt);
        data.extend_from_slice(sender_pub);
        *blake3::hash(&data).as_bytes()
    }

    /// Compute mutual authentication proof for receiver
    pub fn compute_receiver_proof(
        &self,
        session_salt: &[u8; 32],
        receiver_pub: &[u8; 32],
    ) -> [u8; 32] {
        let mut data = Vec::with_capacity(32 + 32 + 32);
        data.extend_from_slice(&self.derive_auth_key());
        data.extend_from_slice(session_salt);
        data.extend_from_slice(receiver_pub);
        *blake3::hash(&data).as_bytes()
    }
}

impl fmt::Display for PairingCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.formatted())
    }
}

impl fmt::Debug for PairingCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PairingCode({})", self.formatted())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pairing_code_generation_and_format() {
        let code = PairingCode::generate().unwrap();
        assert_eq!(code.raw().len(), 12);
        let formatted = code.formatted();
        assert_eq!(formatted.len(), 14); // 12 chars + 2 hyphens
        assert_eq!(&formatted[4..5], "-");
        assert_eq!(&formatted[9..10], "-");
    }

    #[test]
    fn test_pairing_code_parsing_normalization() {
        let parsed = PairingCode::parse("kd7m-9q2p-w8rx").unwrap();
        assert_eq!(parsed.formatted(), "KD7M-9Q2P-W8RX");

        // Test whitespace and typos O -> 0, I -> 1
        let typo = PairingCode::parse(" o d 7 m - 9 q 2 p - w 8 r x ").unwrap();
        assert_eq!(&typo.raw()[0..1], "0");
    }

    #[test]
    fn test_cryptographic_derivation_deterministic() {
        let c1 = PairingCode::parse("KD7M-9Q2P-W8RX").unwrap();
        let c2 = PairingCode::parse("kd7m9q2pw8rx").unwrap();
        assert_eq!(c1.lan_blind_ticket(), c2.lan_blind_ticket());
        assert_eq!(c1.signaling_lookup_hash(), c2.signaling_lookup_hash());
    }
}
