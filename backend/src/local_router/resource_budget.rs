use super::*;

/// Separate from inbound request permits: buffered responses and retained
/// conversation state outlive the small request that produced them.
#[derive(Debug)]
pub(crate) struct RetainedMemoryBudget {
    pool: Arc<Semaphore>,
    permit: Option<OwnedSemaphorePermit>,
}

impl Default for RetainedMemoryBudget {
    fn default() -> Self {
        static POOL: std::sync::OnceLock<Arc<Semaphore>> = std::sync::OnceLock::new();
        Self {
            pool: Arc::clone(POOL.get_or_init(|| {
                Arc::new(Semaphore::new(
                    RETAINED_RESPONSE_BUDGET_BYTES / REQUEST_BODY_BUDGET_UNIT_BYTES,
                ))
            })),
            permit: None,
        }
    }
}

impl RetainedMemoryBudget {
    pub(crate) fn resize(&mut self, bytes: usize) -> Result<()> {
        // As with inbound bodies, reserve for JSON trees and conversion copies.
        let required = bytes
            .saturating_mul(REQUEST_MEMORY_BUDGET_MULTIPLIER)
            .div_ceil(REQUEST_BODY_BUDGET_UNIT_BYTES);
        let held = self
            .permit
            .as_ref()
            .map_or(0, OwnedSemaphorePermit::num_permits);
        if required > held {
            let additional = u32::try_from(required - held).context("响应缓冲预算超出上限")?;
            let permit = Arc::clone(&self.pool)
                .try_acquire_many_owned(additional)
                .context("响应或会话历史缓冲区已满，请压缩上下文或稍后重试")?;
            if let Some(held) = self.permit.as_mut() {
                held.merge(permit);
            } else {
                self.permit = Some(permit);
            }
        } else if required < held {
            if required == 0 {
                self.permit = None;
            } else if let Some(held) = self.permit.as_mut() {
                drop(held.split(held.num_permits() - required));
            }
        }
        Ok(())
    }
}

pub(crate) fn bounded_json_bytes(value: &impl serde::Serialize, limit: usize) -> Result<usize> {
    struct Counter {
        bytes: usize,
        limit: usize,
    }
    impl std::io::Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > self.limit.saturating_sub(self.bytes) {
                return Err(std::io::Error::other("会话历史超过上限，请先压缩上下文"));
            }
            self.bytes += bytes.len();
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut counter = Counter { bytes: 0, limit };
    serde_json::to_writer(&mut counter, value)?;
    Ok(counter.bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retained_budget_is_shared_and_released_on_error_and_drop() {
        let pool = Arc::new(Semaphore::new(4));
        let mut first = RetainedMemoryBudget {
            pool: Arc::clone(&pool),
            permit: None,
        };
        let mut second = RetainedMemoryBudget {
            pool: Arc::clone(&pool),
            permit: None,
        };
        first.resize(REQUEST_BODY_BUDGET_UNIT_BYTES).unwrap();
        assert!(second.resize(1).is_err());
        assert_eq!(pool.available_permits(), 0);
        drop(first);
        second.resize(1).unwrap();
        second.resize(0).unwrap();
        assert_eq!(pool.available_permits(), 4);
    }
}
