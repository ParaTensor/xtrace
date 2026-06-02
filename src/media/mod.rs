//! Langfuse-compatible media storage and reference resolution helpers.

use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine;
use chrono::{Duration, Utc};
use serde_json::Value as JsonValue;
use std::path::{Path, PathBuf};

pub fn media_id_from_sha256_hash(sha256_hash: &str) -> String {
    let url_safe = sha256_hash.replace('+', "-").replace('/', "_");
    url_safe.chars().take(22).collect()
}

pub fn extension_for_content_type(content_type: &str) -> &'static str {
    match content_type {
        "image/png" => "png",
        "image/jpeg" | "image/jpg" => "jpeg",
        "image/webp" => "webp",
        "image/gif" => "gif",
        "application/pdf" => "pdf",
        "audio/mpeg" | "audio/mp3" => "mp3",
        "audio/wav" => "wav",
        _ => "bin",
    }
}

pub fn media_file_path(
    media_dir: &Path,
    project_id: &str,
    media_id: &str,
    content_type: &str,
) -> PathBuf {
    let ext = extension_for_content_type(content_type);
    media_dir.join(project_id).join(format!("{media_id}.{ext}"))
}

pub fn content_url(base: &str, media_id: &str) -> String {
    let base = base.trim_end_matches('/');
    format!("{base}/api/public/media/{media_id}/content")
}

pub fn upload_url(base: &str, media_id: &str) -> String {
    let base = base.trim_end_matches('/');
    format!("{base}/api/public/media/{media_id}/upload")
}

pub fn url_expiry_rfc3339(hours: i64) -> String {
    (Utc::now() + Duration::hours(hours)).to_rfc3339()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaResolveWith {
    Base64DataUri,
    ContentUrl,
}

#[derive(Debug, Clone)]
struct ParsedMediaReference {
    media_id: String,
    content_type: Option<String>,
}

fn parse_media_reference(reference: &str) -> Option<ParsedMediaReference> {
    let s = reference.strip_prefix("@@@langfuseMedia:")?;
    let s = s.strip_suffix("@@@").unwrap_or(s);
    let mut media_id = None;
    let mut content_type = None;
    for part in s.split('|') {
        let Some((k, v)) = part.split_once('=') else {
            continue;
        };
        match k {
            "id" => media_id = Some(v.to_string()),
            "type" => content_type = Some(v.to_string()),
            _ => {}
        }
    }
    Some(ParsedMediaReference {
        media_id: media_id?,
        content_type,
    })
}

fn find_media_references(s: &str) -> Vec<&str> {
    const START: &str = "@@@langfuseMedia:";
    let mut refs = Vec::new();
    let mut pos = 0;
    while let Some(idx) = s[pos..].find(START) {
        let abs = pos + idx;
        let content_start = abs + START.len();
        if let Some(end_rel) = s[content_start..].find("@@@") {
            let end = content_start + end_rel + 3;
            refs.push(&s[abs..end]);
            pos = end;
        } else {
            refs.push(&s[abs..]);
            break;
        }
    }
    refs
}

/// Replace `@@@langfuseMedia:...@@@` tokens in JSON with base64 data URIs or content URLs.
#[allow(clippy::type_complexity)]
pub fn resolve_media_references_in_json(
    value: &JsonValue,
    load_bytes: &dyn Fn(&str) -> Option<(String, Vec<u8>)>,
    resolve_with: MediaResolveWith,
    media_content_url: Option<&dyn Fn(&str) -> String>,
    max_depth: usize,
) -> JsonValue {
    fn walk(
        value: &JsonValue,
        load_bytes: &dyn Fn(&str) -> Option<(String, Vec<u8>)>,
        resolve_with: MediaResolveWith,
        media_content_url: Option<&dyn Fn(&str) -> String>,
        depth: usize,
        max_depth: usize,
    ) -> JsonValue {
        if depth > max_depth {
            return value.clone();
        }
        match value {
            JsonValue::String(s) => {
                if let Ok(parsed) = serde_json::from_str::<JsonValue>(s) {
                    if parsed.is_object() || parsed.is_array() {
                        let resolved = walk(
                            &parsed,
                            load_bytes,
                            resolve_with,
                            media_content_url,
                            depth + 1,
                            max_depth,
                        );
                        return JsonValue::String(resolved.to_string());
                    }
                }
                JsonValue::String(resolve_media_references_in_str(
                    s,
                    load_bytes,
                    resolve_with,
                    media_content_url,
                ))
            }
            JsonValue::Array(arr) => JsonValue::Array(
                arr.iter()
                    .map(|v| {
                        walk(
                            v,
                            load_bytes,
                            resolve_with,
                            media_content_url,
                            depth + 1,
                            max_depth,
                        )
                    })
                    .collect(),
            ),
            JsonValue::Object(map) => JsonValue::Object(
                map.iter()
                    .map(|(k, v)| {
                        (
                            k.clone(),
                            walk(
                                v,
                                load_bytes,
                                resolve_with,
                                media_content_url,
                                depth + 1,
                                max_depth,
                            ),
                        )
                    })
                    .collect(),
            ),
            _ => value.clone(),
        }
    }
    walk(
        value,
        load_bytes,
        resolve_with,
        media_content_url,
        0,
        max_depth,
    )
}

#[allow(clippy::type_complexity)]
pub fn resolve_media_references_in_str(
    s: &str,
    load_bytes: &dyn Fn(&str) -> Option<(String, Vec<u8>)>,
    resolve_with: MediaResolveWith,
    media_content_url: Option<&dyn Fn(&str) -> String>,
) -> String {
    let mut out = s.to_string();
    for reference in find_media_references(s) {
        let Some(parsed) = parse_media_reference(reference) else {
            continue;
        };
        let replacement = if let Some((mut content_type, bytes)) = load_bytes(&parsed.media_id) {
            if let Some(t) = &parsed.content_type {
                content_type = t.clone();
            }
            let b64 = BASE64_STANDARD.encode(&bytes);
            format!("data:{content_type};base64,{b64}")
        } else if resolve_with == MediaResolveWith::ContentUrl {
            if let Some(url_for) = media_content_url {
                url_for(&parsed.media_id)
            } else {
                continue;
            }
        } else {
            continue;
        };
        out = out.replace(reference, &replacement);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn media_id_from_sha256_matches_langfuse_style() {
        let hash = "dGVzdGhhc2gAAAAAAAAAAAAAAAAAAAAAAA==";
        let id = media_id_from_sha256_hash(hash);
        assert_eq!(id.len(), 22);
        assert!(!id.contains('+'));
        assert!(!id.contains('/'));
    }

    #[test]
    fn resolves_media_reference_in_string() {
        let reference = "@@@langfuseMedia:type=image/jpeg|id=test-id-123456789012|source=bytes@@@";
        let input = json!({
            "content": [{
                "type": "image_url",
                "image_url": { "url": reference }
            }]
        });
        let resolved = resolve_media_references_in_json(
            &input,
            &|id| {
                if id == "test-id-123456789012" {
                    Some(("image/jpeg".into(), b"\xff\xd8\xff".to_vec()))
                } else {
                    None
                }
            },
            MediaResolveWith::Base64DataUri,
            None,
            32,
        );
        let url = resolved["content"][0]["image_url"]["url"].as_str().unwrap();
        assert!(url.starts_with("data:image/jpeg;base64,"));
    }

    #[test]
    fn resolves_unclosed_media_reference_to_content_url() {
        let reference =
            "@@@langfuseMedia:type=image/png|id=test-id-123456789012|source=base64_data_uri";
        let resolved = resolve_media_references_in_str(
            reference,
            &|_| None,
            MediaResolveWith::ContentUrl,
            Some(&|id| format!("http://example.test/api/public/media/{id}/content")),
        );
        assert_eq!(
            resolved,
            "http://example.test/api/public/media/test-id-123456789012/content"
        );
    }
}
