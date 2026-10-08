use std::collections::HashMap;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use uuid::Uuid;

const PAIR_LIFETIME: Duration = Duration::from_secs(600);
const SESSION_LIFETIME: Duration = Duration::from_secs(12 * 60 * 60);
const MAX_DEVICES: usize = 8;

struct Device {
    id: String,
    name: String,
    expires: Instant,
}

#[derive(Default)]
pub(super) struct Auth {
    pairing: Option<([u8; 32], Instant)>,
    devices: HashMap<[u8; 32], Device>,
    attempts: Vec<Instant>,
}

fn secret() -> String {
    format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
}

fn digest(value: &str) -> [u8; 32] {
    Sha256::digest(value.as_bytes()).into()
}

impl Auth {
    pub fn pairing_code(&mut self) -> String {
        let code = secret();
        self.pairing = Some((digest(&code), Instant::now() + PAIR_LIFETIME));
        code
    }

    pub fn pair(&mut self, code: &str, name: &str) -> Result<String, &'static str> {
        let now = Instant::now();
        self.attempts
            .retain(|time| now.duration_since(*time) < Duration::from_secs(60));
        if self.attempts.len() >= 10 {
            return Err("配对尝试过于频繁，请稍后重试");
        }
        self.attempts.push(now);
        if name.trim().is_empty() || name.chars().count() > 80 {
            return Err("设备名称须为 1–80 个字符");
        }
        let valid = code.len() == 64
            && self.pairing.as_ref().is_some_and(|(expected, expires)| {
                // Compare fixed-sized digests without exposing a matching prefix.
                let actual = digest(code);
                now < *expires
                    && expected
                        .iter()
                        .zip(actual)
                        .fold(0u8, |diff, (a, b)| diff | (a ^ b))
                        == 0
            });
        if !valid {
            return Err("配对码无效或已过期，请在电脑端重新生成");
        }
        self.devices.retain(|_, device| device.expires > now);
        if self.devices.len() >= MAX_DEVICES {
            return Err("已达到 8 台设备上限，请先在电脑端移除旧设备");
        }
        self.pairing = None;
        let token = secret();
        self.devices.insert(
            digest(&token),
            Device {
                id: Uuid::new_v4().to_string(),
                name: name.trim().to_string(),
                expires: now + SESSION_LIFETIME,
            },
        );
        Ok(token)
    }

    pub fn authorized(&self, token: &str) -> bool {
        token.len() == 64
            && self
                .devices
                .get(&digest(token))
                .is_some_and(|device| device.expires > Instant::now())
    }

    pub fn logout(&mut self, token: &str) {
        self.devices.remove(&digest(token));
    }

    pub fn revoke(&mut self, id: &str) {
        self.devices.retain(|_, device| device.id != id);
    }

    pub fn devices(&self) -> Vec<Value> {
        self.devices
            .values()
            .filter(|device| device.expires > Instant::now())
            .map(|device| json!({"id": device.id, "name": device.name}))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pairing_is_one_time_and_devices_can_be_revoked() {
        let mut auth = Auth::default();
        let code = auth.pairing_code();
        assert!(!auth.authorized(&code));
        let token = auth.pair(&code, "手机").unwrap();
        assert!(auth.authorized(&token));
        assert!(auth.pair(&code, "另一台").is_err());
        let devices = auth.devices();
        assert!(!serde_json::to_string(&devices).unwrap().contains(&token));
        auth.revoke(devices[0]["id"].as_str().unwrap());
        assert!(!auth.authorized(&token));
    }

    #[test]
    fn expired_rotated_and_invalid_codes_fail_closed() {
        let mut auth = Auth::default();
        let old = auth.pairing_code();
        let current = auth.pairing_code();
        assert!(auth.pair(&old, "手机").is_err());
        assert!(auth.pair(&current, "").is_err());
        auth.pairing.as_mut().unwrap().1 = Instant::now();
        assert!(auth.pair(&current, "手机").is_err());
        for _ in 0..10 {
            let _ = auth.pair("wrong", "手机");
        }
        assert!(auth.pair(&current, "手机").unwrap_err().contains("频繁"));
    }

    #[test]
    fn expired_sessions_and_logout_are_rejected() {
        let mut auth = Auth::default();
        let code = auth.pairing_code();
        let token = auth.pair(&code, "手机").unwrap();
        auth.devices.get_mut(&digest(&token)).unwrap().expires = Instant::now();
        assert!(!auth.authorized(&token));
        let code = auth.pairing_code();
        let token = auth.pair(&code, "手机").unwrap();
        auth.logout(&token);
        assert!(!auth.authorized(&token));
    }
}
