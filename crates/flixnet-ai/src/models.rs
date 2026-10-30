//! Model manager: downloads the local embedding model and the ONNX Runtime library
//! into `<config_dir>/models` on first use, verifying every file's SHA-256.
//!
//! * Files are pinned to exact revisions/releases with known hashes; anything that
//!   does not match is deleted, never used.
//! * A verified file gets a `<file>.sha256` marker, so startup does not re-hash
//!   hundreds of megabytes; a file without its marker counts as missing.
//! * Downloads stream into `<file>.part` and are renamed only after verification.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, bail};
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;

/// A downloadable file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Artifact {
    /// What the UI calls it.
    pub label: &'static str,
    pub url: &'static str,
    /// SHA-256 of the downloaded file (the archive, for ONNX Runtime).
    pub sha256: &'static str,
    /// Size of the download in bytes (0 = unknown).
    pub size: u64,
    /// Where it ends up, relative to the models directory.
    pub file: &'static str,
    /// For archives: the member to extract as `file`.
    pub member: Option<&'static str>,
}

/// multilingual-e5-base, int8-quantized ONNX export (Xenova), pinned revision.
pub const E5_MODEL: Artifact = Artifact {
    label: "multilingual-e5-base (int8)",
    url: "https://huggingface.co/Xenova/multilingual-e5-base/resolve/1ec9243030a27d1a115d5c340572074c125b58b2/onnx/model_int8.onnx",
    sha256: "9ddfd8b45086dabc59a7e1bb00463225dace8954962418b240840f2153bc87da",
    size: 278_184_162,
    file: "multilingual-e5-base/model_int8.onnx",
    member: None,
};

pub const E5_TOKENIZER: Artifact = Artifact {
    label: "multilingual-e5-base Tokenizer",
    url: "https://huggingface.co/Xenova/multilingual-e5-base/resolve/1ec9243030a27d1a115d5c340572074c125b58b2/tokenizer.json",
    sha256: "62c24cdc13d4c9952d63718d6c9fa4c287974249e16b7ade6d5a85e7bbb75626",
    size: 17_082_660,
    file: "multilingual-e5-base/tokenizer.json",
    member: None,
};

/// ONNX Runtime version the `ort` crate is built for (and downloaded).
pub const ONNXRUNTIME_VERSION: &str = "1.31.1";

/// The official ONNX Runtime CPU build for this platform, if Microsoft ships one.
pub fn onnxruntime() -> Option<Artifact> {
    if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Some(Artifact {
            label: "ONNX Runtime 1.31.1",
            url: "https://github.com/microsoft/onnxruntime/releases/download/v1.31.1/onnxruntime-linux-x64-1.31.1.tgz",
            sha256: "c849e36a1ef3e22d4ff217b9d1ce4a3cf5b9467b975610913ae959ad5b16bdc9",
            size: 11_413_655,
            file: "onnxruntime-1.31.1/libonnxruntime.so",
            member: Some("onnxruntime-linux-x64-1.31.1/lib/libonnxruntime.so.1.31.1"),
        })
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        Some(Artifact {
            label: "ONNX Runtime 1.31.1",
            url: "https://github.com/microsoft/onnxruntime/releases/download/v1.31.1/onnxruntime-linux-aarch64-1.31.1.tgz",
            sha256: "5317505e2da99be65f8e2775ba3c2afd6d2961fd78aba1bd8f56159f48820c78",
            size: 10_332_381,
            file: "onnxruntime-1.31.1/libonnxruntime.so",
            member: Some("onnxruntime-linux-aarch64-1.31.1/lib/libonnxruntime.so.1.31.1"),
        })
    } else if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        Some(Artifact {
            label: "ONNX Runtime 1.31.1",
            url: "https://github.com/microsoft/onnxruntime/releases/download/v1.31.1/onnxruntime-win-x64-1.31.1.zip",
            sha256: "5869e26bb25314375f2d5ef1877e3ea06b1543d6c710e6e4a1ec2f7177ab4b2c",
            size: 82_929_944,
            file: "onnxruntime-1.31.1/onnxruntime.dll",
            member: Some("onnxruntime-win-x64-1.31.1/lib/onnxruntime.dll"),
        })
    } else {
        None
    }
}

#[derive(Debug, Clone)]
pub struct ModelManager {
    dir: PathBuf,
    /// Overrides the downloaded ONNX Runtime (config or `ORT_DYLIB_PATH`).
    runtime_override: Option<PathBuf>,
}

impl ModelManager {
    pub fn new(dir: impl Into<PathBuf>, runtime_override: Option<PathBuf>) -> Self {
        let runtime_override = runtime_override.or_else(|| {
            std::env::var_os("ORT_DYLIB_PATH")
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
        });
        Self {
            dir: dir.into(),
            runtime_override,
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn path(&self, artifact: &Artifact) -> PathBuf {
        self.dir.join(artifact.file)
    }

    /// The ONNX Runtime library to load, if it is there.
    pub fn runtime_path(&self) -> Option<PathBuf> {
        if let Some(path) = &self.runtime_override {
            return Some(path.clone());
        }
        onnxruntime()
            .filter(|a| self.is_installed(a))
            .map(|a| self.path(&a))
    }

    /// Everything the local model needs that is not there yet.
    pub fn missing(&self) -> Vec<Artifact> {
        let mut missing: Vec<Artifact> = [E5_MODEL, E5_TOKENIZER]
            .into_iter()
            .filter(|a| !self.is_installed(a))
            .collect();
        if self.runtime_override.is_none()
            && let Some(runtime) = onnxruntime()
            && !self.is_installed(&runtime)
        {
            missing.push(runtime);
        }
        missing
    }

    /// Whether the platform can run the local model at all.
    pub fn platform_supported(&self) -> bool {
        self.runtime_override.is_some() || onnxruntime().is_some()
    }

    /// Present and verified earlier (marker with the expected hash).
    pub fn is_installed(&self, artifact: &Artifact) -> bool {
        let path = self.path(artifact);
        let Ok(meta) = std::fs::metadata(&path) else {
            return false;
        };
        if artifact.member.is_none() && meta.len() != artifact.size {
            return false;
        }
        std::fs::read_to_string(marker(&path)).is_ok_and(|m| m.trim() == artifact.sha256)
    }

    /// Downloads, verifies and installs one artifact. `progress(done, total)` is called
    /// as bytes arrive (`total` 0 when unknown).
    pub async fn install(
        &self,
        http: &reqwest::Client,
        artifact: &Artifact,
        mut progress: impl FnMut(u64, u64),
        cancelled: impl Fn() -> bool,
    ) -> anyhow::Result<()> {
        let target = self.path(artifact);
        let parent = target.parent().context("artifact path has no folder")?;
        tokio::fs::create_dir_all(parent)
            .await
            .with_context(|| format!("cannot create {}", parent.display()))?;
        let part = target.with_extension("part");

        let mut response = http
            .get(artifact.url)
            .send()
            .await
            .with_context(|| format!("download of {} failed", artifact.label))?;
        if !response.status().is_success() {
            bail!(
                "download of {} failed: HTTP {}",
                artifact.label,
                response.status()
            );
        }
        let total = response.content_length().unwrap_or(artifact.size);
        let mut file = tokio::fs::File::create(&part)
            .await
            .with_context(|| format!("cannot write {}", part.display()))?;
        let mut hasher = Sha256::new();
        let mut done = 0_u64;
        let result: anyhow::Result<()> = async {
            while let Some(chunk) = response.chunk().await? {
                if cancelled() {
                    bail!("download cancelled");
                }
                hasher.update(&chunk);
                file.write_all(&chunk).await?;
                done += chunk.len() as u64;
                progress(done, total);
            }
            file.flush().await?;
            Ok(())
        }
        .await;
        drop(file);
        if let Err(err) = result {
            let _ = tokio::fs::remove_file(&part).await;
            return Err(err);
        }
        let actual = hex::encode(hasher.finalize());
        if actual != artifact.sha256 {
            let _ = tokio::fs::remove_file(&part).await;
            bail!(
                "{} is corrupt (SHA-256 {actual}, expected {})",
                artifact.label,
                artifact.sha256
            );
        }

        match artifact.member {
            None => {
                tokio::fs::rename(&part, &target).await?;
            }
            Some(member) => {
                let (archive, out, member) = (part.clone(), target.clone(), member.to_owned());
                let url = artifact.url;
                tokio::task::spawn_blocking(move || extract(&archive, &member, &out, url))
                    .await??;
                let _ = tokio::fs::remove_file(&part).await;
            }
        }
        tokio::fs::write(marker(&target), artifact.sha256).await?;
        Ok(())
    }

    /// Removes downloaded models and runtime (frees the space, disables local AI).
    pub async fn remove_all(&self) -> anyhow::Result<()> {
        if tokio::fs::try_exists(&self.dir).await.unwrap_or(false) {
            tokio::fs::remove_dir_all(&self.dir).await?;
        }
        Ok(())
    }
}

fn marker(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".sha256");
    path.with_file_name(name)
}

/// Copies one member out of a `.tgz` or `.zip` archive (verified beforehand).
fn extract(archive: &Path, member: &str, out: &Path, url: &str) -> anyhow::Result<()> {
    let tmp = out.with_extension("extracting");
    let mut bytes = Vec::new();
    if url.ends_with(".zip") {
        let file = std::fs::File::open(archive)?;
        let mut zip = zip::ZipArchive::new(file).context("reading the zip archive")?;
        let mut entry = zip
            .by_name(member)
            .with_context(|| format!("{member} missing in the archive"))?;
        entry.read_to_end(&mut bytes)?;
    } else {
        let file = std::fs::File::open(archive)?;
        let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(file));
        let mut found = false;
        for entry in tar.entries()? {
            let mut entry = entry?;
            if entry.path()?.to_string_lossy() == member {
                entry.read_to_end(&mut bytes)?;
                found = true;
                break;
            }
        }
        if !found {
            bail!("{member} missing in the archive");
        }
    }
    std::fs::write(&tmp, &bytes)?;
    std::fs::rename(&tmp, out)?;
    Ok(())
}

/// HTTP client for large downloads: no overall timeout, but stalled reads fail.
pub fn download_client() -> reqwest::Client {
    crate::http::install_crypto_provider();
    reqwest::Client::builder()
        .user_agent(concat!("Flixnet/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(15))
        .read_timeout(Duration::from_secs(60))
        .build()
        .expect("HTTP client configuration is valid")
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn leak(s: String) -> &'static str {
        Box::leak(s.into_boxed_str())
    }

    #[tokio::test]
    async fn downloads_are_verified_and_marked() {
        let server = MockServer::start().await;
        let body = b"pretend this is a model".to_vec();
        Mock::given(method("GET"))
            .and(path("/model.onnx"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(body.clone()))
            .mount(&server)
            .await;
        let dir = tempfile::tempdir().unwrap();
        let manager = ModelManager::new(dir.path(), Some("/opt/ort/libonnxruntime.so".into()));
        let good = Artifact {
            label: "Testmodell",
            url: leak(format!("{}/model.onnx", server.uri())),
            sha256: leak(hex::encode(Sha256::digest(&body))),
            size: body.len() as u64,
            file: "test/model.onnx",
            member: None,
        };
        assert!(!manager.is_installed(&good));
        let mut seen = Vec::new();
        manager
            .install(
                &download_client(),
                &good,
                |done, total| seen.push((done, total)),
                || false,
            )
            .await
            .unwrap();
        assert!(manager.is_installed(&good));
        assert_eq!(seen.last(), Some(&(body.len() as u64, body.len() as u64)));
        assert_eq!(std::fs::read(manager.path(&good)).unwrap(), body);

        // A wrong hash is rejected and leaves nothing behind.
        let bad = Artifact {
            sha256: "00",
            file: "test/other.onnx",
            ..good
        };
        let err = manager
            .install(&download_client(), &bad, |_, _| {}, || false)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("corrupt"), "{err}");
        assert!(!manager.path(&bad).exists());
        assert!(!manager.path(&bad).with_extension("part").exists());
        assert!(!manager.is_installed(&bad));

        // A tampered file loses its installed state (size check).
        std::fs::write(manager.path(&good), b"x").unwrap();
        assert!(!manager.is_installed(&good));
        // The runtime override counts as installed runtime.
        assert_eq!(
            manager.runtime_path(),
            Some(PathBuf::from("/opt/ort/libonnxruntime.so"))
        );
        assert!(manager.missing().iter().all(|a| a.member.is_none()));
    }

    #[tokio::test]
    async fn runtime_archives_are_extracted() {
        // A .tgz with the library inside a versioned folder, like Microsoft ships it.
        let mut tgz = Vec::new();
        {
            let gz = flate2::write::GzEncoder::new(&mut tgz, flate2::Compression::fast());
            let mut builder = tar::Builder::new(gz);
            let data = b"\x7fELF fake library";
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder
                .append_data(&mut header, "ort-x/lib/libonnxruntime.so.9", &data[..])
                .unwrap();
            builder.into_inner().unwrap().finish().unwrap();
        }
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/ort.tgz"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(tgz.clone()))
            .mount(&server)
            .await;
        let dir = tempfile::tempdir().unwrap();
        let manager = ModelManager::new(dir.path(), None);
        let runtime = Artifact {
            label: "Runtime",
            url: leak(format!("{}/ort.tgz", server.uri())),
            sha256: leak(hex::encode(Sha256::digest(&tgz))),
            size: 0,
            file: "ort/libonnxruntime.so",
            member: Some("ort-x/lib/libonnxruntime.so.9"),
        };
        manager
            .install(&download_client(), &runtime, |_, _| {}, || false)
            .await
            .unwrap();
        assert_eq!(
            std::fs::read(manager.path(&runtime)).unwrap(),
            b"\x7fELF fake library"
        );
        assert!(manager.is_installed(&runtime));
        assert!(!dir.path().join("ort/libonnxruntime.part").exists());
    }
}
