use std::{
    collections::HashMap,
    io::Write,
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

use base64::{Engine, engine::general_purpose::STANDARD};
use serde::Deserialize;
use serde_json::{Value, json};
use tempfile::NamedTempFile;

use crate::codey_plugins;

const MAX_CHUNK_BYTES: usize = 256 * 1024;
const MAX_UPLOADS: usize = 4;
const UPLOAD_LIFETIME: Duration = Duration::from_secs(3600);

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Chunk {
    id: Option<String>,
    name: String,
    offset: u64,
    content_base64: String,
    complete: bool,
}

struct Upload {
    file: NamedTempFile,
    name: String,
    size: u64,
    ready: bool,
    touched: Instant,
}

#[derive(Default)]
struct Uploads {
    files: HashMap<String, Upload>,
}

impl Uploads {
    fn receive(&mut self, value: Value) -> Result<Value, String> {
        self.receive_at(value, Instant::now())
    }

    fn receive_at(&mut self, value: Value, now: Instant) -> Result<Value, String> {
        self.files
            .retain(|_, upload| now.saturating_duration_since(upload.touched) < UPLOAD_LIFETIME);
        let chunk: Chunk = serde_json::from_value(value)
            .map_err(|error| format!("插件包上传参数无效：{error}"))?;
        if chunk.name.len() > 255
            || chunk.name.contains(['/', '\\'])
            || !chunk.name.ends_with(".codey-plugin")
        {
            return Err("仅支持本地 .codey-plugin 插件包".into());
        }
        if chunk.content_base64.len() > MAX_CHUNK_BYTES.div_ceil(3) * 4 {
            return Err("插件包上传分块超过大小限制".into());
        }
        let bytes = STANDARD
            .decode(&chunk.content_base64)
            .map_err(|_| "插件包上传内容损坏，请重试".to_string())?;
        if bytes.is_empty() || bytes.len() > MAX_CHUNK_BYTES {
            return Err("插件包上传分块为空或超过大小限制".into());
        }
        let id = match chunk.id {
            Some(id) => id,
            None => {
                if chunk.offset != 0 || self.files.len() >= MAX_UPLOADS {
                    return Err("插件包上传起点无效或正在处理的文件过多，请稍后重试".into());
                }
                let id = uuid::Uuid::new_v4().to_string();
                let file = tempfile::Builder::new()
                    .prefix("codey-import-")
                    .suffix(".codey-plugin")
                    .tempfile()
                    .map_err(|error| error.to_string())?;
                self.files.insert(
                    id.clone(),
                    Upload {
                        file,
                        name: chunk.name.clone(),
                        size: 0,
                        ready: false,
                        touched: now,
                    },
                );
                id
            }
        };
        let result = (|| {
            let upload = self
                .files
                .get_mut(&id)
                .ok_or("插件包上传已过期，请重新检查")?;
            if upload.ready || upload.name != chunk.name || upload.size != chunk.offset {
                return Err("插件包上传顺序无效，请重新检查".to_string());
            }
            if upload.size + bytes.len() as u64 > codey_plugins::MAX_PACKAGE {
                return Err("插件包超过 64 MiB".to_string());
            }
            upload
                .file
                .write_all(&bytes)
                .map_err(|error| error.to_string())?;
            upload.size += bytes.len() as u64;
            upload.touched = now;
            if !chunk.complete {
                return Ok(json!({"uploadId": id}));
            }
            upload.file.flush().map_err(|error| error.to_string())?;
            let inspection = codey_plugins::inspect(upload.file.path())?;
            upload.ready = true;
            let mut preview =
                serde_json::to_value(inspection).map_err(|error| error.to_string())?;
            preview["uploadId"] = id.clone().into();
            Ok(preview)
        })();
        if result.is_err() {
            self.files.remove(&id);
        }
        result
    }
}

fn uploads() -> &'static Mutex<Uploads> {
    static UPLOADS: OnceLock<Mutex<Uploads>> = OnceLock::new();
    UPLOADS.get_or_init(|| Mutex::new(Uploads::default()))
}

pub(super) fn receive(value: Value) -> Result<Value, String> {
    uploads()
        .lock()
        .map_err(|_| "插件包上传状态不可用，请重启 Codey".to_string())?
        .receive(value)
}

pub(super) fn discard(id: &str) -> Result<Value, String> {
    uploads()
        .lock()
        .map_err(|_| "插件包上传状态不可用，请重启 Codey".to_string())?
        .files
        .remove(id);
    Ok(json!({"status": "ok"}))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(id: Option<&str>, offset: u64, bytes: &[u8], complete: bool) -> Value {
        json!({"id": id, "name": "demo.codey-plugin", "offset": offset, "contentBase64": STANDARD.encode(bytes), "complete": complete})
    }

    #[test]
    fn chunks_are_bounded_ordered_and_cleaned_after_corruption() {
        let mut uploads = Uploads::default();
        let first = uploads
            .receive(chunk(None, 0, b"not a zip", false))
            .unwrap();
        let id = first["uploadId"].as_str().unwrap();
        let path = uploads.files[id].file.path().to_path_buf();
        assert!(path.is_file());
        let error = uploads
            .receive(chunk(Some(id), 9, b" still broken", true))
            .unwrap_err();
        assert!(error.contains("ZIP"), "{error}");
        assert!(uploads.files.is_empty());
        assert!(!path.exists());
        let first = uploads.receive(chunk(None, 0, b"first", false)).unwrap();
        let id = first["uploadId"].as_str().unwrap();
        assert!(
            uploads
                .receive(chunk(Some(id), 0, b"replayed", false))
                .unwrap_err()
                .contains("顺序")
        );
        assert!(uploads.files.is_empty());
    }

    #[test]
    fn invalid_uploads_do_not_create_temporary_files() {
        let mut uploads = Uploads::default();
        for name in [
            "demo.zip",
            "demo.CODEY-PLUGIN",
            "../demo.codey-plugin",
            "..\\demo.codey-plugin",
        ] {
            let mut value = chunk(None, 0, b"data", false);
            value["name"] = name.into();
            assert!(uploads.receive(value).is_err());
        }
        for value in [
            chunk(None, 1, b"data", false),
            chunk(None, 0, b"", false),
            chunk(None, 0, &vec![0; MAX_CHUNK_BYTES + 1], false),
        ] {
            assert!(uploads.receive(value).is_err());
        }
        let mut value = chunk(None, 0, b"data", false);
        value["contentBase64"] = "not base64!".into();
        assert!(uploads.receive(value).is_err());
        assert!(uploads.files.is_empty());
    }

    #[test]
    fn upload_size_capacity_and_expiry_are_enforced() {
        let mut uploads = Uploads::default();
        let first = uploads.receive(chunk(None, 0, b"data", false)).unwrap();
        let id = first["uploadId"].as_str().unwrap();
        uploads.files.get_mut(id).unwrap().size = codey_plugins::MAX_PACKAGE;
        assert!(
            uploads
                .receive(chunk(Some(id), codey_plugins::MAX_PACKAGE, b"x", false))
                .unwrap_err()
                .contains("64 MiB")
        );
        let now = Instant::now();
        for _ in 0..MAX_UPLOADS {
            uploads
                .receive_at(chunk(None, 0, b"data", false), now)
                .unwrap();
        }
        assert!(
            uploads
                .receive_at(
                    chunk(None, 0, b"data", false),
                    now + UPLOAD_LIFETIME - Duration::from_nanos(1),
                )
                .is_err()
        );
        uploads
            .receive_at(chunk(None, 0, b"data", false), now + UPLOAD_LIFETIME)
            .unwrap();
        assert_eq!(uploads.files.len(), 1);
    }

    #[test]
    fn valid_upload_reuses_inspection_and_install_digest_validation() {
        let entry = if cfg!(target_os = "macos") {
            "lib.dylib"
        } else if cfg!(windows) {
            "lib.dll"
        } else {
            "lib.so"
        };
        let manifest = json!({
            "id": "test.upload", "name": "Upload", "version": "1.0.0", "abiVersion": 1,
            "platform": std::env::consts::OS, "arch": std::env::consts::ARCH,
            "entry": entry, "librarySha256": crate::fs_util::sha256_hex(b"library")
        });
        let mut archive = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        for (name, content) in [
            ("manifest.json", serde_json::to_vec(&manifest).unwrap()),
            (entry, b"library".to_vec()),
            ("config.json", b"{}\n".to_vec()),
        ] {
            archive
                .start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            archive.write_all(&content).unwrap();
        }
        let bytes = archive.finish().unwrap().into_inner();
        let mut uploads = Uploads::default();
        let first = uploads
            .receive(chunk(None, 0, &bytes[..100], false))
            .unwrap();
        let id = first["uploadId"].as_str().unwrap();
        let preview = uploads
            .receive(chunk(Some(id), 100, &bytes[100..], true))
            .unwrap();
        assert_eq!(preview["manifest"]["id"], "test.upload");
        assert_eq!(preview["sha256"], crate::fs_util::sha256_hex(&bytes));
        assert!(uploads.files[id].ready);
        let path = uploads.files[id].file.path();
        let inspected = codey_plugins::inspect(path).unwrap();
        assert_eq!(inspected.sha256, preview["sha256"]);
        assert!(
            codey_plugins::install(path, &"0".repeat(64))
                .err()
                .unwrap()
                .contains("检查后发生变化")
        );
    }

    #[test]
    fn cancelling_upload_deletes_staging_and_is_idempotent() {
        let first = receive(chunk(None, 0, b"partial", false)).unwrap();
        let id = first["uploadId"].as_str().unwrap();
        let path = uploads().lock().unwrap().files[id]
            .file
            .path()
            .to_path_buf();
        assert!(path.is_file());
        discard(id).unwrap();
        discard(id).unwrap();
        assert!(!path.exists());
    }
}
