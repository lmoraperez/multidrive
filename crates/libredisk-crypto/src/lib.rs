//! Cifrado de conocimiento cero para LibreDisk.
//!
//! Cada trozo se sella de forma independiente con XChaCha20-Poly1305. La clave maestra se
//! deriva de una frase de paso con Argon2id y nunca se escribe en disco.
//!
//! El identificador del trozo se usa como datos autenticados adicionales (AAD): un trozo
//! sellado no puede reubicarse en otra posicion del volumen sin que el descifrado falle.

use argon2::{Algorithm, Argon2, Params, Version};
use chacha20poly1305::aead::{Aead, Generate, Payload};
use chacha20poly1305::{KeyInit, XChaCha20Poly1305, XNonce};
use zeroize::{Zeroize, ZeroizeOnDrop};

/// Longitud del nonce de XChaCha20-Poly1305.
pub const NONCE_LEN: usize = 24;
/// Longitud de la etiqueta de autenticacion de Poly1305.
pub const TAG_LEN: usize = 16;
/// Longitud de la sal recomendada para la derivacion de clave.
pub const SALT_LEN: usize = 16;

/// Sobrecoste en bytes que el sellado anade a un trozo.
pub const SEAL_OVERHEAD: usize = NONCE_LEN + TAG_LEN;

#[derive(Debug, thiserror::Error)]
pub enum CryptoError {
    #[error("no se pudo derivar la clave maestra: {0}")]
    KeyDerivation(String),
    #[error("parametros de Argon2id invalidos: {0}")]
    Params(String),
    #[error("el trozo sellado esta truncado: {0} bytes, se necesitan al menos {1}")]
    Truncated(usize, usize),
    #[error(
        "fallo la autenticacion del trozo: clave incorrecta, dato corrupto o AAD que no corresponde"
    )]
    Authentication,
}

/// Clave maestra de 32 bytes. Se borra de memoria al destruirse.
#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub struct MasterKey([u8; 32]);

impl MasterKey {
    /// Construye una clave a partir de material en bruto. Preferir [`MasterKey::derive`].
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Deriva la clave maestra de una frase de paso con Argon2id.
    ///
    /// Los parametros por defecto (19 MiB, 2 pasadas) son los recomendados por OWASP; son
    /// deliberadamente costosos para frenar el ataque por diccionario sobre la frase.
    pub fn derive(passphrase: &str, salt: &[u8]) -> Result<Self, CryptoError> {
        Self::derive_with(
            passphrase,
            salt,
            Params::DEFAULT_M_COST,
            Params::DEFAULT_T_COST,
        )
    }

    /// Igual que [`MasterKey::derive`] pero con coste explicito. Util en pruebas, donde los
    /// parametros por defecto harian los tests insoportablemente lentos.
    pub fn derive_with(
        passphrase: &str,
        salt: &[u8],
        m_cost: u32,
        t_cost: u32,
    ) -> Result<Self, CryptoError> {
        let params = Params::new(m_cost, t_cost, Params::DEFAULT_P_COST, Some(32))
            .map_err(|e| CryptoError::Params(e.to_string()))?;
        let argon = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);

        let mut out = [0u8; 32];
        argon
            .hash_password_into(passphrase.as_bytes(), salt, &mut out)
            .map_err(|e| CryptoError::KeyDerivation(e.to_string()))?;
        Ok(Self(out))
    }

    fn cipher(&self) -> XChaCha20Poly1305 {
        XChaCha20Poly1305::new((&self.0).into())
    }

    /// Sella un trozo. Devuelve `nonce || ciphertext || tag`.
    ///
    /// `aad` queda autenticado pero no cifrado; LibreDisk pasa ahi el identificador del
    /// trozo, de modo que reubicar un trozo valido en otro sitio se detecta al abrirlo.
    pub fn seal(&self, plaintext: &[u8], aad: &[u8]) -> Result<Vec<u8>, CryptoError> {
        let nonce = XNonce::generate();
        let ciphertext = self
            .cipher()
            .encrypt(
                &nonce,
                Payload {
                    msg: plaintext,
                    aad,
                },
            )
            .map_err(|_| CryptoError::Authentication)?;

        let mut out = Vec::with_capacity(NONCE_LEN + ciphertext.len());
        out.extend_from_slice(&nonce);
        out.extend_from_slice(&ciphertext);
        Ok(out)
    }

    /// Abre un trozo sellado por [`MasterKey::seal`].
    pub fn open(&self, sealed: &[u8], aad: &[u8]) -> Result<Vec<u8>, CryptoError> {
        if sealed.len() < SEAL_OVERHEAD {
            return Err(CryptoError::Truncated(sealed.len(), SEAL_OVERHEAD));
        }
        let (nonce, ciphertext) = sealed.split_at(NONCE_LEN);
        self.cipher()
            .decrypt(
                &XNonce::try_from(nonce).expect("split_at garantiza NONCE_LEN bytes"),
                Payload {
                    msg: ciphertext,
                    aad,
                },
            )
            .map_err(|_| CryptoError::Authentication)
    }
}

impl std::fmt::Debug for MasterKey {
    /// Nunca se imprime el material de la clave, ni siquiera en trazas de depuracion.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("MasterKey(<oculta>)")
    }
}

/// Genera una sal aleatoria para [`MasterKey::derive`].
pub fn generate_salt() -> [u8; SALT_LEN] {
    let nonce = XNonce::generate();
    let mut salt = [0u8; SALT_LEN];
    salt.copy_from_slice(&nonce[..SALT_LEN]);
    salt
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parametros baratos: los de produccion son deliberadamente lentos.
    fn test_key(pass: &str) -> MasterKey {
        MasterKey::derive_with(pass, b"sal-de-prueba-16", 8, 1).unwrap()
    }

    #[test]
    fn ida_y_vuelta() {
        let key = test_key("correcto caballo bateria grapa");
        let sealed = key.seal(b"contenido del trozo", b"chunk-001").unwrap();
        assert_eq!(
            key.open(&sealed, b"chunk-001").unwrap(),
            b"contenido del trozo"
        );
    }

    #[test]
    fn el_texto_cifrado_no_revela_el_claro() {
        let key = test_key("frase");
        let claro = b"DATOS MUY SECRETOS DEL USUARIO";
        let sealed = key.seal(claro, b"chunk-001").unwrap();
        assert!(
            !sealed.windows(claro.len()).any(|w| w == claro),
            "el texto claro aparece literal en la salida sellada"
        );
        assert_eq!(sealed.len(), claro.len() + SEAL_OVERHEAD);
    }

    #[test]
    fn nonce_distinto_en_cada_sellado() {
        let key = test_key("frase");
        let a = key.seal(b"mismo contenido", b"chunk-001").unwrap();
        let b = key.seal(b"mismo contenido", b"chunk-001").unwrap();
        assert_ne!(
            a, b,
            "dos sellados del mismo dato deben diferir (nonce aleatorio)"
        );
    }

    #[test]
    fn clave_incorrecta_falla() {
        let sealed = test_key("buena").seal(b"dato", b"chunk-001").unwrap();
        assert!(matches!(
            test_key("mala").open(&sealed, b"chunk-001"),
            Err(CryptoError::Authentication)
        ));
    }

    #[test]
    fn aad_distinto_falla() {
        // Esta es la defensa contra reubicar un trozo valido en otra posicion del volumen.
        let key = test_key("frase");
        let sealed = key.seal(b"dato", b"chunk-001").unwrap();
        assert!(matches!(
            key.open(&sealed, b"chunk-999"),
            Err(CryptoError::Authentication)
        ));
    }

    #[test]
    fn dato_manipulado_falla() {
        let key = test_key("frase");
        let mut sealed = key.seal(b"dato importante", b"chunk-001").unwrap();
        let last = sealed.len() - 1;
        sealed[last] ^= 0x01;
        assert!(matches!(
            key.open(&sealed, b"chunk-001"),
            Err(CryptoError::Authentication)
        ));
    }

    #[test]
    fn sellado_truncado_falla_con_claridad() {
        let key = test_key("frase");
        assert!(matches!(
            key.open(&[0u8; 8], b"chunk-001"),
            Err(CryptoError::Truncated(8, _))
        ));
    }

    #[test]
    fn la_derivacion_es_determinista_y_depende_de_la_sal() {
        let a = MasterKey::derive_with("frase", b"sal-aaaaaaaaaaaa", 8, 1).unwrap();
        let b = MasterKey::derive_with("frase", b"sal-aaaaaaaaaaaa", 8, 1).unwrap();
        let c = MasterKey::derive_with("frase", b"sal-bbbbbbbbbbbb", 8, 1).unwrap();
        assert_eq!(a.0, b.0);
        assert_ne!(a.0, c.0);
    }

    #[test]
    fn la_clave_no_se_filtra_en_debug() {
        let dbg = format!("{:?}", test_key("frase"));
        assert_eq!(dbg, "MasterKey(<oculta>)");
    }
}
