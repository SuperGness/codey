use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};

// Keep the phone composer and the desktop frame budget in sync (see images.ts).
const MAX_IMAGES: usize = 4;
const MAX_IMAGE_BYTES: usize = 4 * 1024 * 1024;

pub(super) fn message_input(args: &Value) -> Result<Vec<Value>, String> {
    let text = args["text"]
        .as_str()
        .filter(|text| text.len() <= 100_000)
        .ok_or("请输入有效消息，最多 100000 字节")?;
    let images = match args.get("images") {
        None => &[][..],
        Some(value) => value
            .as_array()
            .filter(|images| images.len() <= MAX_IMAGES)
            .ok_or("图片列表无效，每条消息最多添加 4 张图片")?
            .as_slice(),
    };
    let mut input = Vec::new();
    if !text.trim().is_empty() {
        input.push(json!({"type":"text", "text":text, "text_elements":[]}));
    }
    let mut total = 0;
    for image in images {
        let url = image["url"].as_str().ok_or("图片数据无效，请重新选择")?;
        let (mime, encoded) = url
            .strip_prefix("data:")
            .and_then(|data| data.split_once(";base64,"))
            .filter(|(mime, _)| {
                matches!(
                    *mime,
                    "image/png" | "image/jpeg" | "image/gif" | "image/webp"
                )
            })
            .ok_or("仅支持 PNG、JPEG、GIF 或 WebP 图片数据")?;
        if encoded.len() > MAX_IMAGE_BYTES.div_ceil(3) * 4 {
            return Err("图片总大小不能超过 4 MB".into());
        }
        let bytes = STANDARD.decode(encoded).map_err(|_| "图片编码无效")?;
        total += bytes.len();
        if total > MAX_IMAGE_BYTES {
            return Err("图片总大小不能超过 4 MB".into());
        }
        let valid = match mime {
            "image/png" => bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
            "image/jpeg" => bytes.starts_with(&[0xff, 0xd8, 0xff]),
            "image/gif" => bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a"),
            "image/webp" => bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP"),
            _ => false,
        };
        if !valid {
            return Err("图片内容与格式不匹配，请重新选择".into());
        }
        // Inline image input is shared by start, steer and native creation. The
        // desktop's attachments context is reserved for its local file handles.
        input.push(json!({"type":"image", "url":url}));
    }
    if input.is_empty() {
        return Err("请输入消息或添加图片".into());
    }
    Ok(input)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_text_images_and_image_only_messages_without_changing_order() {
        for (mime, bytes) in [
            ("png", b"\x89PNG\r\n\x1a\n".as_slice()),
            ("jpeg", &[0xff, 0xd8, 0xff]),
            ("gif", b"GIF89a".as_slice()),
            ("webp", b"RIFF0000WEBP".as_slice()),
        ] {
            let url = format!("data:image/{mime};base64,{}", STANDARD.encode(bytes));
            for text in ["查看图片", " "] {
                let input = message_input(&json!({"text":text,"images":[{"url":url}]})).unwrap();
                assert_eq!(input.last().unwrap(), &json!({"type":"image","url":url}));
                assert_eq!(input.len(), if text.trim().is_empty() { 1 } else { 2 });
            }
        }
        assert_eq!(
            message_input(&json!({"text":"test"})).unwrap()[0]["text"],
            "test"
        );
    }

    #[test]
    fn rejects_empty_malformed_external_or_mislabeled_images() {
        for args in [
            json!({"text":" "}),
            json!({"text":null}),
            json!({"text":"a".repeat(100_001)}),
            json!({"text":"ok","images":null}),
            json!({"text":"ok","images":{}}),
            json!({"text":"ok","images":[{}]}),
            json!({"text":"ok","images":[{"url":1}]}),
            json!({"text":"ok","images":vec![json!({});5]}),
        ] {
            assert!(message_input(&args).is_err());
        }
        for url in [
            "https://example.test/a.png",
            "file:///private/image.png",
            "data:image/svg+xml;base64,PHN2Zz4=",
            "data:image/png;base64,!",
            "data:image/png;base64,",
            "data:image/png;base64,YQ==",
        ] {
            let error = message_input(&json!({"text":"ok","images":[{"url":url}]})).unwrap_err();
            assert!(!error.contains(url));
        }
    }

    #[test]
    fn enforces_decoded_total_even_when_client_size_is_forged() {
        let mut bytes = vec![0; MAX_IMAGE_BYTES];
        bytes[..3].copy_from_slice(&[0xff, 0xd8, 0xff]);
        let image =
            json!({"url":format!("data:image/jpeg;base64,{}",STANDARD.encode(&bytes)),"size":0});
        assert!(message_input(&json!({"text":"","images":[image]})).is_ok());
        assert!(
            message_input(&json!({"text":"","images":[image,image]}))
                .unwrap_err()
                .contains("4 MB")
        );
        bytes.push(0);
        let oversized = json!({"url":format!("data:image/jpeg;base64,{}",STANDARD.encode(&bytes))});
        assert!(
            message_input(&json!({"text":"","images":[oversized]}))
                .unwrap_err()
                .contains("4 MB")
        );
    }
}
