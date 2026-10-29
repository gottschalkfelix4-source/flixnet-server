//! Image cache: originals (local files or downloaded provider images) and resized
//! variants on disk, plus blurhash placeholders.

use std::io::Cursor;
use std::path::{Path, PathBuf};

use anyhow::Context;
use flixnet_db::metadata::Image;
use image::{DynamicImage, ImageFormat, imageops::FilterType};
use sha2::{Digest, Sha256};

/// Widths served to clients; requests snap up to the next one so the cache stays small.
pub const WIDTHS: &[u32] = &[92, 185, 342, 500, 780, 1280, 1920];

#[derive(Clone)]
pub struct ImageCache {
    dir: PathBuf,
    http: reqwest::Client,
}

/// A cached variant ready to send.
pub struct Variant {
    pub path: PathBuf,
    pub content_type: &'static str,
    /// Computed when the original was first processed (store it on the image row).
    pub blurhash: Option<(u32, u32, String)>,
}

pub fn snap_width(requested: Option<u32>) -> Option<u32> {
    let requested = requested?;
    Some(
        WIDTHS
            .iter()
            .copied()
            .find(|w| *w >= requested)
            .unwrap_or(*WIDTHS.last().expect("non-empty")),
    )
}

impl ImageCache {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            http: crate::http::client(),
        }
    }

    /// Stable cache key: the URL, or a local path plus its modification time.
    async fn key(&self, image: &Image) -> anyhow::Result<String> {
        let source = match (&image.path, &image.url) {
            (Some(path), _) => {
                let mtime = tokio::fs::metadata(path)
                    .await
                    .with_context(|| format!("image file {path} is gone"))?
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map_or(0, |d| d.as_secs());
                format!("{path}|{mtime}")
            }
            (None, Some(url)) => url.clone(),
            (None, None) => anyhow::bail!("image {} has neither path nor url", image.id),
        };
        Ok(hex::encode(&Sha256::digest(source.as_bytes())[..16]))
    }

    /// Original bytes (downloaded once for remote images).
    async fn original(&self, image: &Image, folder: &Path) -> anyhow::Result<Vec<u8>> {
        if let Some(path) = &image.path {
            return Ok(tokio::fs::read(path).await?);
        }
        let cached = folder.join("original");
        if let Ok(bytes) = tokio::fs::read(&cached).await {
            return Ok(bytes);
        }
        let url = image.url.as_deref().context("remote image without url")?;
        let response = self.http.get(url).send().await?.error_for_status()?;
        let bytes = response.bytes().await?.to_vec();
        write_atomic(&cached, &bytes).await?;
        Ok(bytes)
    }

    /// Returns the (possibly resized) image, creating cache files on first use.
    pub async fn variant(&self, image: &Image, width: Option<u32>) -> anyhow::Result<Variant> {
        let key = self.key(image).await?;
        let folder = self.dir.join(&key[..2]).join(&key);
        tokio::fs::create_dir_all(&folder).await?;
        let width = snap_width(width);
        let keep_alpha = image.kind == "logo";
        let ext = if keep_alpha { "png" } else { "jpg" };
        let content_type = if keep_alpha {
            "image/png"
        } else {
            "image/jpeg"
        };
        let target = folder.join(format!(
            "{}.{ext}",
            width.map_or("full".to_owned(), |w| w.to_string())
        ));

        if tokio::fs::try_exists(&target).await.unwrap_or(false) {
            return Ok(Variant {
                path: target,
                content_type,
                blurhash: None,
            });
        }

        let bytes = self.original(image, &folder).await?;
        let wants_blurhash = image.blurhash.is_none();
        let (encoded, blurhash) = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
            let decoded = image::load_from_memory(&bytes).context("not a decodable image")?;
            let blurhash = wants_blurhash.then(|| blurhash_of(&decoded)).flatten();
            let resized = match width {
                Some(w) if w < decoded.width() => decoded.resize(w, u32::MAX, FilterType::Triangle),
                _ => decoded,
            };
            let mut out = Cursor::new(Vec::new());
            if keep_alpha {
                resized.write_to(&mut out, ImageFormat::Png)?;
            } else {
                let rgb = DynamicImage::ImageRgb8(resized.to_rgb8());
                let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 85);
                encoder.encode_image(&rgb)?;
            }
            Ok((out.into_inner(), blurhash))
        })
        .await??;
        write_atomic(&target, &encoded).await?;
        Ok(Variant {
            path: target,
            content_type,
            blurhash,
        })
    }
}

/// 4x3 component blurhash of a small thumbnail, plus the original size.
fn blurhash_of(image: &DynamicImage) -> Option<(u32, u32, String)> {
    let thumb = image.thumbnail(32, 32).to_rgba8();
    let hash = blurhash::encode(4, 3, thumb.width(), thumb.height(), thumb.as_raw()).ok()?;
    Some((image.width(), image.height(), hash))
}

/// Writes via a temporary file so concurrent readers never see half a file.
async fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    tokio::fs::write(&tmp, bytes).await?;
    match tokio::fs::rename(&tmp, path).await {
        Ok(()) => Ok(()),
        // Another request won the race; its file is just as good.
        Err(_) if path.exists() => {
            let _ = tokio::fs::remove_file(&tmp).await;
            Ok(())
        }
        Err(err) => Err(err),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn widths_snap_up() {
        assert_eq!(snap_width(None), None);
        assert_eq!(snap_width(Some(300)), Some(342));
        assert_eq!(snap_width(Some(342)), Some(342));
        assert_eq!(snap_width(Some(5000)), Some(1920));
    }
}
