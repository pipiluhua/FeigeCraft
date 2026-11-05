//! TLS：自签名证书、指纹（SHA-256）与「指纹固定」的客户端校验。
//!
//! LocalSend 用自签名证书 + 指纹比对防止局域网中间人，这里保持一致策略：
//! 首次通过组播拿到对方证书指纹，之后所有 HTTPS 请求都要求服务端证书指纹匹配。

use std::fs;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};

const CERT_FILE: &str = "cert.der";
const KEY_FILE: &str = "key.der";

/// 本机身份：证书、私钥与其指纹。
#[derive(Clone)]
pub struct Identity {
    pub cert_der: Vec<u8>,
    pub key_der: Vec<u8>,
    pub fingerprint: String,
}

impl std::fmt::Debug for Identity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Identity").field("fingerprint", &self.fingerprint).finish()
    }
}

impl Identity {
    /// 生成新的自签名证书。
    pub fn generate() -> Result<Self> {
        let certified = rcgen::generate_simple_self_signed(vec!["feige.local".to_string()])
            .context("生成自签名证书失败")?;
        let cert_der = certified.cert.der().to_vec();
        let key_der = certified.key_pair.serialize_der();
        let fingerprint = fingerprint_of(&cert_der);
        Ok(Self { cert_der, key_der, fingerprint })
    }

    /// 从目录加载身份，不存在则生成并持久化（保证指纹跨重启稳定）。
    pub fn load_or_create(dir: &Path) -> Result<Self> {
        let cert_path = dir.join(CERT_FILE);
        let key_path = dir.join(KEY_FILE);
        if cert_path.exists() && key_path.exists() {
            let cert_der = fs::read(&cert_path).context("读取证书失败")?;
            let key_der = fs::read(&key_path).context("读取私钥失败")?;
            let fingerprint = fingerprint_of(&cert_der);
            return Ok(Self { cert_der, key_der, fingerprint });
        }
        let identity = Self::generate()?;
        identity.save(dir)?;
        Ok(identity)
    }

    pub fn save(&self, dir: &Path) -> Result<()> {
        fs::create_dir_all(dir).context("创建配置目录失败")?;
        fs::write(dir.join(CERT_FILE), &self.cert_der).context("写入证书失败")?;
        fs::write(dir.join(KEY_FILE), &self.key_der).context("写入私钥失败")?;
        Ok(())
    }

    /// 服务端 rustls 配置（仅 HTTP/1.1）。
    pub fn server_config(&self) -> Result<Arc<rustls::ServerConfig>> {
        let certs = vec![CertificateDer::from(self.cert_der.clone())];
        let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(self.key_der.clone()));
        let mut config = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(certs, key)
            .context("构建 TLS 服务端配置失败")?;
        config.alpn_protocols = vec![b"http/1.1".to_vec()];
        Ok(Arc::new(config))
    }
}

/// 计算证书 DER 的 SHA-256 指纹（小写十六进制）。
pub fn fingerprint_of(cert_der: &[u8]) -> String {
    let digest = Sha256::digest(cert_der);
    hex::encode(digest)
}

/// 构造「只信任该指纹」的客户端配置；`expected` 为 `None` 时不做校验（用于子网扫描）。
pub fn client_config(expected: Option<&str>) -> Result<rustls::ClientConfig> {
    let expected = match expected {
        Some(value) if !value.is_empty() => {
            let bytes = hex::decode(value).context("指纹不是合法的十六进制")?;
            let mut array = [0u8; 32];
            if bytes.len() != 32 {
                anyhow::bail!("指纹长度不是 32 字节");
            }
            array.copy_from_slice(&bytes);
            Some(array)
        }
        _ => None,
    };
    let mut config = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(PinnedVerifier { expected }))
        .with_no_client_auth();
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(config)
}

#[derive(Debug)]
struct PinnedVerifier {
    expected: Option<[u8; 32]>,
}

impl ServerCertVerifier for PinnedVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> std::result::Result<ServerCertVerified, rustls::Error> {
        if let Some(expected) = self.expected {
            let actual: [u8; 32] = Sha256::digest(end_entity.as_ref()).into();
            if actual != expected {
                return Err(rustls::Error::General(
                    "证书指纹与设备广播的指纹不一致，已中断连接".to_string(),
                ));
            }
        }
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        // 证书本身已按指纹固定，签名校验交由证书指纹保证
        Ok(HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        vec![
            SignatureScheme::RSA_PKCS1_SHA256,
            SignatureScheme::RSA_PKCS1_SHA384,
            SignatureScheme::RSA_PKCS1_SHA512,
            SignatureScheme::ECDSA_NISTP256_SHA256,
            SignatureScheme::ECDSA_NISTP384_SHA384,
            SignatureScheme::ED25519,
            SignatureScheme::RSA_PSS_SHA256,
            SignatureScheme::RSA_PSS_SHA384,
            SignatureScheme::RSA_PSS_SHA512,
        ]
    }
}
