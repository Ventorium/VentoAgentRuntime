//! OCR post-pass for images embedded in documents (docx/doc/rtf/odt/pptx/
//! epub assets, PDF inline images).
//!
//! Every unique image (deduped by content hash of the original bytes) is sent
//! to the configured provider, downscaled first when oversized, with bounded
//! concurrency and a per-image timeout. Failures never abort the conversion:
//! the image still renders as a `![…]()` marker with no annotation block.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use sha2::{Digest, Sha256};
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use super::{OcrOptions, OcrProvider, OcrRequest};
use crate::ConvertOptions;

/// Knobs for the embedded-image OCR pass, resolved from [`ConvertOptions`].
#[derive(Clone, Debug)]
pub(crate) struct ImageOcrConfig {
    /// Whether the pass runs at all.
    pub enabled: bool,
    /// Images whose long side exceeds this are downscaled to it before OCR.
    pub max_dimension: u32,
    /// Maximum in-flight OCR requests.
    pub concurrency: usize,
    /// Per-image budget for the provider call (downscaling excluded).
    pub timeout: Duration,
}

impl From<&ConvertOptions> for ImageOcrConfig {
    fn from(options: &ConvertOptions) -> Self {
        Self {
            enabled: options.image_ocr.unwrap_or(true),
            max_dimension: options.image_ocr_max_dimension.unwrap_or(1024),
            concurrency: options.image_ocr_concurrency.unwrap_or(4).clamp(1, 64),
            timeout: Duration::from_millis(options.image_ocr_timeout_ms.unwrap_or(60_000)),
        }
    }
}

/// Result of recognizing one unique image.
#[derive(Clone, Debug)]
pub(crate) enum OcrOutcome {
    /// Recognized text (already joined from pages by the provider).
    Text(String),
    /// The provider errored or the per-image timeout elapsed.
    Failed,
}

/// One image up for recognition: bytes plus a file name whose extension
/// feeds the provider's mime inference.
pub(crate) struct OcrImage {
    pub bytes: Vec<u8>,
    pub file_name: String,
}

/// SHA-256 of the original bytes: the dedup and outcome-lookup key.
pub(crate) fn content_hash(bytes: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher.finalize().into()
}

/// Extension for well-known image mime types.
fn ext_for_mime(mime: &str) -> Option<&'static str> {
    match mime {
        "image/jpeg" => Some("jpg"),
        "image/png" => Some("png"),
        "image/webp" => Some("webp"),
        "image/gif" => Some("gif"),
        "image/bmp" => Some("bmp"),
        "image/tiff" => Some("tiff"),
        "image/jp2" | "image/jpx" => Some("jp2"),
        _ => None,
    }
}

/// Make sure a file name carries an image extension matching its mime type,
/// so the provider's extension-based inference works for extensionless
/// names like PDF `image-3`.
pub(crate) fn ensure_image_extension(name: &str, mime: &str) -> String {
    match ext_for_mime(mime) {
        Some(ext) => replace_extension(name, ext),
        None => name.to_owned(),
    }
}

/// Recognize `images`, deduped by SHA-256 of the original bytes. Images that
/// are skipped (pass disabled, duplicate, no provider) simply have no entry
/// in the returned map; callers fall back to a plain `图片` alt.
pub(crate) async fn run_image_ocr(
    images: &[OcrImage],
    config: &ImageOcrConfig,
    provider: Option<Arc<dyn OcrProvider>>,
    language: Option<&str>,
) -> HashMap<[u8; 32], OcrOutcome> {
    let mut outcomes = HashMap::new();
    let Some(provider) = provider else {
        return outcomes;
    };
    if !config.enabled || images.is_empty() {
        return outcomes;
    }

    // Dedup by content hash, keeping the first file name for each unique
    // image (same bytes → same text, one provider call).
    let mut unique: Vec<([u8; 32], OcrImage)> = Vec::new();
    let mut seen: HashMap<[u8; 32], ()> = HashMap::new();
    for image in images {
        let hash = content_hash(&image.bytes);
        if seen.insert(hash, ()).is_some() {
            continue;
        }
        unique.push((
            hash,
            OcrImage {
                bytes: image.bytes.clone(),
                file_name: image.file_name.clone(),
            },
        ));
    }

    let semaphore = Arc::new(Semaphore::new(config.concurrency));
    let mut set: JoinSet<([u8; 32], OcrOutcome)> = JoinSet::new();
    for (hash, image) in unique {
        let permit = semaphore
            .clone()
            .acquire_owned()
            .await
            .expect("semaphore is never closed");
        let provider = Arc::clone(&provider);
        let language = language.map(str::to_owned);
        let timeout = config.timeout;
        let max_dimension = config.max_dimension;
        set.spawn(async move {
            let _permit = permit;
            // Decode + resize is CPU work; keep it off the async workers.
            let prepared = tokio::task::spawn_blocking(move || {
                let OcrImage { bytes, file_name } = image;
                match downscale(&bytes, max_dimension) {
                    Some((bytes, ext)) => (bytes, replace_extension(&file_name, ext)),
                    None => (bytes, file_name),
                }
            })
            .await;
            let (bytes, file_name) = match prepared {
                Ok(pair) => pair,
                Err(error) => {
                    log::warn!("image OCR resize task failed: {error}");
                    return (hash, OcrOutcome::Failed);
                }
            };
            let recognize = provider.recognize(
                OcrRequest {
                    bytes,
                    file_name,
                    language: language.clone(),
                },
                OcrOptions {
                    language,
                    page_numbers: Vec::new(),
                    extra: serde_json::Map::new(),
                },
            );
            let outcome = match tokio::time::timeout(timeout, recognize).await {
                Ok(Ok(output)) => OcrOutcome::Text(output.markdown),
                Ok(Err(error)) => {
                    log::warn!(
                        "image OCR failed ({code}): {message}",
                        code = error.code,
                        message = error.message
                    );
                    OcrOutcome::Failed
                }
                Err(_) => {
                    log::warn!("image OCR timed out after {}ms", timeout.as_millis());
                    OcrOutcome::Failed
                }
            };
            (hash, outcome)
        });
    }
    while let Some(joined) = set.join_next().await {
        match joined {
            Ok((hash, outcome)) => {
                outcomes.insert(hash, outcome);
            }
            Err(error) => log::warn!("image OCR task panicked: {error}"),
        }
    }
    outcomes
}

/// Downscale `bytes` so its long side is at most `max`, keeping the aspect
/// ratio. Smaller images and undecodable formats (WMF/EMF/SVG) pass through
/// untouched (`None`) so the provider can still try the original bytes;
/// re-encoded images come back with the extension matching their new format.
fn downscale(bytes: &[u8], max: u32) -> Option<(Vec<u8>, &'static str)> {
    let decoded = image::load_from_memory(bytes).ok()?;
    let (width, height) = (decoded.width(), decoded.height());
    let long = width.max(height);
    if long <= max || long == 0 {
        return None;
    }
    let scale = f64::from(max) / f64::from(long);
    let new_width = ((f64::from(width) * scale).round() as u32).max(1);
    let new_height = ((f64::from(height) * scale).round() as u32).max(1);
    let resized = image::imageops::resize(
        &decoded,
        new_width,
        new_height,
        image::imageops::FilterType::Lanczos3,
    );
    let use_png = decoded.color().has_alpha()
        || matches!(image::guess_format(bytes), Ok(image::ImageFormat::Png));
    let mut encoded = Vec::new();
    let encoded_ok = if use_png {
        let encoder = image::codecs::png::PngEncoder::new(&mut encoded);
        resized.write_with_encoder(encoder).is_ok()
    } else {
        let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut encoded, 85);
        resized.write_with_encoder(encoder).is_ok()
    };
    encoded_ok.then_some((encoded, if use_png { "png" } else { "jpg" }))
}

/// Point `name` at the given extension so the provider's extension-based
/// mime inference matches the actual bytes.
fn replace_extension(name: &str, ext: &str) -> String {
    match name.rfind('.') {
        Some(idx)
            if !name[idx + 1..].contains('/')
                && (1..=5).contains(&name[idx + 1..].len())
                && name[idx + 1..].bytes().all(|b| b.is_ascii_alphanumeric()) =>
        {
            format!("{}.{}", &name[..idx], ext)
        }
        _ => format!("{name}.{ext}"),
    }
}

/// Compose the alt text for an embedded image: fixed `图片` prefix (greppable)
/// plus the document's own alt when present. `number` is the image's
/// document-order figure number; recognized text never goes in here — it is
/// emitted as a separate [`ocr_annotation`] block, because arbitrary OCR text
/// (brackets, pipes, line starts) would otherwise break the surrounding
/// Markdown.
pub(crate) fn compose_alt(original_alt: &str, number: Option<usize>) -> String {
    let mut alt = match number {
        Some(number) => format!("图片{number}"),
        None => String::from("图片"),
    };
    let original = flatten(original_alt);
    if !original.is_empty() {
        alt.push('：');
        push_escaped(&mut alt, &original);
    }
    alt
}

/// The annotation block that follows an OCR'd image: the recognized text
/// quoted line by line, so nothing in it can restructure the document around
/// it. `number` matches the image's alt label.
pub(crate) fn ocr_annotation(number: usize, outcome: &OcrOutcome) -> String {
    let body = match outcome {
        OcrOutcome::Text(text) if !text.trim().is_empty() => text.trim(),
        _ => "（OCR识别失败）",
    };
    let mut out = format!("> 图片{number}的OCR解析结果如下：\n>");
    for line in body.lines() {
        out.push('\n');
        if line.trim_end().is_empty() {
            out.push('>');
        } else {
            out.push_str("> ");
            out.push_str(line.trim_end());
        }
    }
    out
}

/// Collapse all whitespace runs (including newlines) to single spaces: alt
/// text must stay single-line and grep-friendly.
fn flatten(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Escape the bracket delimiters so the alt cannot close the `![…]` label
/// early; the rest needs no escaping inside a label.
fn push_escaped(out: &mut String, text: &str) {
    for ch in text.chars() {
        if ch == '[' || ch == ']' {
            out.push('\\');
        }
        out.push(ch);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct CountingProvider {
        calls: AtomicUsize,
        delay: Option<Duration>,
    }

    #[async_trait]
    impl OcrProvider for CountingProvider {
        fn name(&self) -> &'static str {
            "counting"
        }
        async fn recognize(
            &self,
            _request: OcrRequest,
            _options: OcrOptions,
        ) -> Result<super::super::OcrOutput, super::super::OcrError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if let Some(delay) = self.delay {
                tokio::time::sleep(delay).await;
            }
            Ok(super::super::OcrOutput {
                markdown: "识别结果".into(),
                confidence: None,
                provider: "counting".into(),
            })
        }
    }

    fn config() -> ImageOcrConfig {
        ImageOcrConfig {
            enabled: true,
            max_dimension: 1024,
            concurrency: 4,
            timeout: Duration::from_secs(60),
        }
    }

    fn image(bytes: &[u8], name: &str) -> OcrImage {
        OcrImage {
            bytes: bytes.to_vec(),
            file_name: name.into(),
        }
    }

    #[tokio::test]
    async fn dedups_identical_bytes() {
        let provider = Arc::new(CountingProvider {
            calls: AtomicUsize::new(0),
            delay: None,
        });
        let images = vec![
            image(b"aaa", "a.png"),
            image(b"bbb", "b.png"),
            image(b"aaa", "c.png"),
        ];
        let outcomes = run_image_ocr(&images, &config(), Some(provider.clone()), None).await;
        assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
        assert_eq!(outcomes.len(), 2);
        assert!(matches!(
            outcomes.values().next(),
            Some(OcrOutcome::Text(_))
        ));
    }

    #[tokio::test]
    async fn timeout_marks_failure() {
        let provider = Arc::new(CountingProvider {
            calls: AtomicUsize::new(0),
            delay: Some(Duration::from_millis(200)),
        });
        let mut cfg = config();
        cfg.timeout = Duration::from_millis(20);
        let outcomes = run_image_ocr(&[image(b"aaa", "a.png")], &cfg, Some(provider), None).await;
        assert!(matches!(outcomes.values().next(), Some(OcrOutcome::Failed)));
    }

    #[tokio::test]
    async fn no_provider_is_a_noop() {
        let outcomes = run_image_ocr(&[image(b"aaa", "a.png")], &config(), None, None).await;
        assert!(outcomes.is_empty());
    }

    #[test]
    fn downscale_respects_cap_and_aspect() {
        // 2000x1000 RGB PNG → long side capped at 1000, aspect kept, PNG out.
        let img = image::RgbImage::from_pixel(2000, 1000, image::Rgb([200, 30, 30]));
        let mut bytes = Vec::new();
        let encoder = image::codecs::png::PngEncoder::new(&mut bytes);
        image::DynamicImage::ImageRgb8(img)
            .write_with_encoder(encoder)
            .unwrap();
        let (out, ext) = downscale(&bytes, 1000).expect("oversized image is resized");
        assert_eq!(ext, "png");
        let decoded = image::load_from_memory(&out).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (1000, 500));
    }

    #[test]
    fn downscale_leaves_small_images_untouched() {
        let img = image::RgbImage::from_pixel(64, 64, image::Rgb([1, 2, 3]));
        let mut bytes = Vec::new();
        let encoder = image::codecs::png::PngEncoder::new(&mut bytes);
        image::DynamicImage::ImageRgb8(img)
            .write_with_encoder(encoder)
            .unwrap();
        assert!(downscale(&bytes, 1024).is_none());
    }

    #[test]
    fn downscale_passes_undecodable_bytes_through() {
        assert!(downscale(b"not-an-image", 1024).is_none());
    }

    #[test]
    fn extension_replacement() {
        assert_eq!(replace_extension("image-1", "jpg"), "image-1.jpg");
        assert_eq!(replace_extension("image-1.wmf", "png"), "image-1.png");
        assert_eq!(
            replace_extension("word/media/image1.png", "png"),
            "word/media/image1.png"
        );
        assert_eq!(replace_extension("a.b.c.png", "jpg"), "a.b.c.jpg");
    }

    #[test]
    fn compose_alt_templates() {
        assert_eq!(compose_alt("", Some(3)), "图片3");
        assert_eq!(compose_alt("", None), "图片");
        // Original alt is kept, whitespace collapsed, brackets escaped.
        assert_eq!(compose_alt("a [b]", Some(3)), "图片3：a \\[b\\]");
    }

    #[test]
    fn annotation_quotes_every_line() {
        let annotation = ocr_annotation(3, &OcrOutcome::Text("# 标题\n\n| a | b |".into()));
        assert_eq!(
            annotation,
            "> 图片3的OCR解析结果如下：\n>\n> # 标题\n>\n> | a | b |"
        );
        // Nothing in the quoted block can open a heading or a table at the
        // document level.
        for line in annotation.lines() {
            assert!(line == ">" || line.starts_with("> "), "unquoted: {line}");
        }
    }

    #[test]
    fn annotation_reports_failure_and_empty_text() {
        assert_eq!(
            ocr_annotation(1, &OcrOutcome::Failed),
            "> 图片1的OCR解析结果如下：\n>\n> （OCR识别失败）"
        );
        assert_eq!(
            ocr_annotation(1, &OcrOutcome::Text("  \n".into())),
            "> 图片1的OCR解析结果如下：\n>\n> （OCR识别失败）"
        );
    }
}
