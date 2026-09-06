use super::*;

#[derive(Debug)]
pub(crate) struct HttpRequest {
    pub(crate) method: String,
    pub(crate) path: String,
    pub(crate) headers: Vec<(String, String)>,
    pub(crate) body: Vec<u8>,
    pub(crate) _body_budget_permit: Option<OwnedSemaphorePermit>,
}

#[derive(Debug)]
pub(crate) struct PendingHttpRequest {
    pub(crate) request: HttpRequest,
    pub(crate) content_length: usize,
}

#[cfg(test)]
pub(crate) async fn read_http_request<R>(stream: &mut R) -> Result<HttpRequest>
where
    R: tokio::io::AsyncRead + Unpin,
{
    read_http_request_with_budget(stream, None).await
}

#[derive(Debug)]
pub(crate) struct RequestBodyBudgetUnavailable;

impl std::fmt::Display for RequestBodyBudgetUnavailable {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Codey 本地路由请求缓冲预算不足")
    }
}

impl std::error::Error for RequestBodyBudgetUnavailable {}

#[derive(Debug)]
pub(crate) struct UnsupportedRequestContentEncoding;

impl std::fmt::Display for UnsupportedRequestContentEncoding {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Responses 请求体仅支持 identity 或 zstd Content-Encoding")
    }
}

impl std::error::Error for UnsupportedRequestContentEncoding {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ResponsesRequestBodyEncoding {
    Identity,
    Zstd,
}

#[cfg(test)]
pub(crate) async fn read_http_request_with_budget<R>(
    stream: &mut R,
    body_budget: Option<&Arc<Semaphore>>,
) -> Result<HttpRequest>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let pending = read_http_request_head(stream).await?;
    read_http_request_body_with_budget(stream, pending, body_budget).await
}

pub(crate) async fn read_http_request_head<R>(stream: &mut R) -> Result<PendingHttpRequest>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut buffer = Vec::new();
    let header_end = loop {
        let mut chunk = [0_u8; 4096];
        let read = stream
            .read(&mut chunk)
            .await
            .context("读取 Codey 本地路由请求失败")?;
        if read == 0 {
            anyhow::bail!("请求在 HTTP 头读取完成前断开");
        }
        buffer.extend_from_slice(&chunk[..read]);
        if let Some(index) = find_header_end(&buffer) {
            if index > MAX_HEADER_BYTES {
                anyhow::bail!("HTTP 请求头超过 Codey 本地路由安全上限");
            }
            break index;
        }
        // Keep at most the three bytes that may be the beginning of the
        // terminating CRLFCRLF sequence beyond the header byte limit. A read
        // may also contain body bytes, which must not count as header bytes.
        if buffer.len() > MAX_HEADER_BYTES.saturating_add(3) {
            anyhow::bail!("HTTP 请求头超过 Codey 本地路由安全上限");
        }
    };
    let header_text = std::str::from_utf8(&buffer[..header_end]).context("HTTP 头不是 UTF-8")?;
    let mut lines = header_text.split("\r\n");
    let request_line = lines.next().unwrap_or_default();
    let mut request_parts = request_line.split_whitespace();
    let method = request_parts
        .next()
        .ok_or_else(|| anyhow::anyhow!("HTTP 请求缺少方法"))?
        .to_string();
    let raw_path = request_parts
        .next()
        .ok_or_else(|| anyhow::anyhow!("HTTP 请求缺少路径"))?;
    let path = raw_path.split('?').next().unwrap_or(raw_path).to_string();
    let mut headers = Vec::new();
    let mut content_length = 0_usize;
    let mut saw_content_length = false;
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let name = name.trim().to_string();
        let value = value.trim().to_string();
        if name.eq_ignore_ascii_case("content-length") {
            let parsed = value.parse::<usize>().context("HTTP Content-Length 无效")?;
            if saw_content_length && parsed != content_length {
                anyhow::bail!("HTTP 请求包含冲突的 Content-Length");
            }
            saw_content_length = true;
            content_length = parsed;
        }
        if name.eq_ignore_ascii_case("transfer-encoding") && !value.eq_ignore_ascii_case("identity")
        {
            anyhow::bail!("Codey 本地路由不接受分块请求体");
        }
        headers.push((name, value));
    }
    if content_length > MAX_REQUEST_BYTES {
        anyhow::bail!("请求体超过 Codey 本地路由安全上限");
    }
    let body_start = header_end + 4;
    let buffered_body = buffer.get(body_start..).unwrap_or_default();
    let mut body = Vec::with_capacity(buffered_body.len().min(content_length));
    body.extend_from_slice(&buffered_body[..buffered_body.len().min(content_length)]);
    Ok(PendingHttpRequest {
        request: HttpRequest {
            method,
            path,
            headers,
            body,
            _body_budget_permit: None,
        },
        content_length,
    })
}

pub(crate) async fn read_http_request_body_with_budget<R>(
    stream: &mut R,
    mut pending: PendingHttpRequest,
    body_budget: Option<&Arc<Semaphore>>,
) -> Result<HttpRequest>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let body_budget_permit = body_budget
        .map(|body_budget| acquire_request_body_budget(body_budget, pending.content_length))
        .transpose()?
        .flatten();
    pending.request._body_budget_permit = body_budget_permit;
    // `content_length` is already bounded by MAX_REQUEST_BYTES and covered by
    // the budget permit, so one reservation replaces repeated Vec doubling.
    pending.request.body.reserve_exact(
        pending
            .content_length
            .saturating_sub(pending.request.body.len()),
    );
    let mut chunk = [0_u8; 8192];
    while pending.request.body.len() < pending.content_length {
        let remaining = pending.content_length - pending.request.body.len();
        let read_length = remaining.min(chunk.len());
        let read = stream
            .read(&mut chunk[..read_length])
            .await
            .context("读取 Codey 本地路由请求体失败")?;
        if read == 0 {
            anyhow::bail!("请求体读取完成前连接断开");
        }
        pending.request.body.extend_from_slice(&chunk[..read]);
    }
    pending.request.body.truncate(pending.content_length);
    Ok(pending.request)
}

pub(crate) fn responses_request_body_encoding(
    request: &HttpRequest,
) -> Result<ResponsesRequestBodyEncoding> {
    let mut encoding = None;
    for (name, value) in &request.headers {
        if !name.eq_ignore_ascii_case(CONTENT_ENCODING.as_str()) {
            continue;
        }
        for token in value.split(',') {
            let token = token.trim();
            if token.is_empty() || encoding.replace(token).is_some() {
                return Err(anyhow::Error::new(UnsupportedRequestContentEncoding));
            }
        }
    }
    match encoding {
        None => Ok(ResponsesRequestBodyEncoding::Identity),
        Some(encoding) if encoding.eq_ignore_ascii_case("identity") => {
            Ok(ResponsesRequestBodyEncoding::Identity)
        }
        Some(encoding) if encoding.eq_ignore_ascii_case("zstd") => {
            Ok(ResponsesRequestBodyEncoding::Zstd)
        }
        Some(_) => Err(anyhow::Error::new(UnsupportedRequestContentEncoding)),
    }
}

pub(crate) async fn decode_responses_request_body(
    request: &mut HttpRequest,
    body_budget: &Arc<Semaphore>,
) -> Result<Vec<u8>> {
    let encoding = responses_request_body_encoding(request)?;
    request
        .headers
        .retain(|(name, _)| !name.eq_ignore_ascii_case(CONTENT_ENCODING.as_str()));
    let encoded = std::mem::take(&mut request.body);
    if encoding == ResponsesRequestBodyEncoding::Identity {
        return Ok(encoded);
    }

    reserve_request_body_budget_for_decompression(request, body_budget)?;
    let decoded = tokio::task::spawn_blocking(move || decode_zstd_request_body(encoded))
        .await
        .context("等待 Responses zstd 请求体解压任务失败")??;
    shrink_request_body_budget(request, decoded.len())?;
    Ok(decoded)
}

pub(crate) async fn parse_responses_request_body(
    encoded: Vec<u8>,
) -> Result<(Vec<u8>, serde_json::Result<Value>)> {
    if encoded.len() < REQUEST_JSON_OFFLOAD_BYTES {
        let parsed = serde_json::from_slice(&encoded);
        return Ok((encoded, parsed));
    }
    tokio::task::spawn_blocking(move || {
        let parsed = serde_json::from_slice(&encoded);
        (encoded, parsed)
    })
    .await
    .context("等待大型 Responses JSON 解析任务失败")
}

pub(crate) fn decode_zstd_request_body(encoded: Vec<u8>) -> Result<Vec<u8>> {
    let mut decoder = zstd::stream::read::Decoder::new(Cursor::new(encoded))
        .context("初始化 Responses zstd 请求体解码器失败")?;
    decoder
        .window_log_max(25)
        .context("限制 Responses zstd 请求体解压窗口失败")?;
    let mut decoded = Vec::new();
    decoder
        .take((MAX_REQUEST_BYTES as u64).saturating_add(1))
        .read_to_end(&mut decoded)
        .context("解压 Responses zstd 请求体失败")?;
    if decoded.len() > MAX_REQUEST_BYTES {
        anyhow::bail!("解压后的 Responses 请求体超过 Codey 本地路由安全上限");
    }
    Ok(decoded)
}

pub(crate) fn request_body_budget_permit_count(wire_bytes: usize) -> Result<usize> {
    if wire_bytes > MAX_REQUEST_BYTES {
        anyhow::bail!("请求体超过 Codey 本地路由安全上限");
    }
    let estimated_memory = wire_bytes.saturating_mul(REQUEST_MEMORY_BUDGET_MULTIPLIER);
    Ok(estimated_memory.div_ceil(REQUEST_BODY_BUDGET_UNIT_BYTES))
}

pub(crate) fn reserve_request_body_budget_for_decompression(
    request: &mut HttpRequest,
    body_budget: &Arc<Semaphore>,
) -> Result<()> {
    let required = request_body_budget_permit_count(MAX_REQUEST_BYTES)?;
    let held = request
        ._body_budget_permit
        .as_ref()
        .map(OwnedSemaphorePermit::num_permits)
        .unwrap_or(0);
    let additional = required.saturating_sub(held);
    if additional == 0 {
        return Ok(());
    }
    let additional = u32::try_from(additional).context("请求体解压预算超出内部上限")?;
    let permit = Arc::clone(body_budget)
        .try_acquire_many_owned(additional)
        .map_err(|_| anyhow::Error::new(RequestBodyBudgetUnavailable))?;
    if let Some(held) = request._body_budget_permit.as_mut() {
        held.merge(permit);
    } else {
        request._body_budget_permit = Some(permit);
    }
    Ok(())
}

pub(crate) fn shrink_request_body_budget(
    request: &mut HttpRequest,
    decoded_bytes: usize,
) -> Result<()> {
    let desired = request_body_budget_permit_count(decoded_bytes)?;
    let Some(mut held) = request._body_budget_permit.take() else {
        return Ok(());
    };
    if desired == 0 {
        return Ok(());
    }
    if desired > held.num_permits() {
        anyhow::bail!("Responses 请求体解压预算不足");
    }
    if desired == held.num_permits() {
        request._body_budget_permit = Some(held);
        return Ok(());
    }
    let retained = held
        .split(desired)
        .context("缩减 Responses 请求体解压预算失败")?;
    drop(held);
    request._body_budget_permit = Some(retained);
    Ok(())
}

pub(crate) fn find_header_end(buffer: &[u8]) -> Option<usize> {
    buffer.windows(4).position(|window| window == b"\r\n\r\n")
}

pub(crate) fn acquire_request_body_budget(
    body_budget: &Arc<Semaphore>,
    wire_bytes: usize,
) -> Result<Option<OwnedSemaphorePermit>> {
    let permits = request_body_budget_permit_count(wire_bytes)?;
    if permits == 0 {
        return Ok(None);
    }
    let permits = u32::try_from(permits).context("请求体缓冲预算超出内部上限")?;
    Arc::clone(body_budget)
        .try_acquire_many_owned(permits)
        .map(Some)
        .map_err(|_| anyhow::Error::new(RequestBodyBudgetUnavailable))
}
