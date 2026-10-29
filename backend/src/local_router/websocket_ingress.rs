use super::*;
use std::future::Future;
use std::io;
use std::pin::Pin;
use std::task::{Context as TaskContext, Poll};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

// Fixed per-connection storage is separate from the shared request working set.
// Large decoder buffers are discarded after delivering a complete message.
const WEBSOCKET_READ_BUFFER_BYTES: usize = 8 * 1024;
const UNBUDGETED_CONTROL_MESSAGE_BYTES: usize = 4 * 1024;

/// Inspect a bounded frame header before tungstenite can reserve its payload.
/// Reads never cross a frame boundary, so a completed message can safely replace
/// the decoder without losing the beginning of the following frame.
pub(crate) struct WebSocketIngress {
    stream: Option<TcpStream>,
    budget: Arc<Semaphore>,
    handshake: bool,
    handshake_tail: u32,
    handshake_bytes: usize,
    prefetched: Box<[u8]>,
    prefetched_start: usize,
    prefetched_end: usize,
    header: [u8; 14],
    header_read: usize,
    header_sent: usize,
    header_len: usize,
    payload_remaining: usize,
    data_frame: bool,
    final_frame: bool,
    fragmented: bool,
    message_bytes: usize,
    largest_frame: usize,
    message_complete: bool,
    permit: Option<OwnedSemaphorePermit>,
    read_deadline: Option<Pin<Box<tokio::time::Sleep>>>,
    reading: bool,
    #[cfg(test)]
    write_remaining: Option<usize>,
}

impl WebSocketIngress {
    pub(crate) fn new(stream: TcpStream, budget: Arc<Semaphore>) -> Self {
        Self::empty(Some(stream), budget, true)
    }

    fn empty(stream: Option<TcpStream>, budget: Arc<Semaphore>, handshake: bool) -> Self {
        let prefetched = if stream.is_some() {
            vec![0; WEBSOCKET_READ_BUFFER_BYTES].into_boxed_slice()
        } else {
            Box::default()
        };
        Self {
            stream,
            budget,
            handshake,
            handshake_tail: 0,
            handshake_bytes: 0,
            prefetched,
            prefetched_start: 0,
            prefetched_end: 0,
            header: [0; 14],
            header_read: 0,
            header_sent: 0,
            header_len: 2,
            payload_remaining: 0,
            data_frame: false,
            final_frame: false,
            fragmented: false,
            message_bytes: 0,
            largest_frame: 0,
            message_complete: false,
            permit: None,
            read_deadline: None,
            reading: false,
            #[cfg(test)]
            write_remaining: None,
        }
    }

    fn start_read(&mut self) {
        if !self.reading {
            let deadline = tokio::time::Instant::now() + REQUEST_READ_TIMEOUT;
            if let Some(timer) = self.read_deadline.as_mut() {
                timer.as_mut().reset(deadline);
            } else {
                self.read_deadline = Some(Box::pin(tokio::time::sleep_until(deadline)));
            }
            self.reading = true;
        }
    }

    fn poll_deadline(&mut self, cx: &mut TaskContext<'_>) -> io::Result<()> {
        if self.reading
            && self
                .read_deadline
                .as_mut()
                .is_some_and(|timer| timer.as_mut().poll(cx).is_ready())
        {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "WebSocket 请求正文读取超时",
            ));
        }
        Ok(())
    }

    fn prepare_header(&mut self) -> io::Result<()> {
        let first = self.header[0];
        let opcode = first & 0x0f;
        let final_frame = first & 0x80 != 0;
        if first & 0x70 != 0 || self.header[1] & 0x80 == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "无效 WebSocket 帧标志",
            ));
        }
        let length = match self.header[1] & 0x7f {
            126 => u64::from(u16::from_be_bytes(self.header[2..4].try_into().unwrap())),
            127 => u64::from_be_bytes(self.header[2..10].try_into().unwrap()),
            length => u64::from(length),
        };
        let length = usize::try_from(length)
            .ok()
            .filter(|length| *length <= MAX_REQUEST_BYTES)
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "WebSocket 请求正文超过上限")
            })?;
        self.data_frame = opcode < 8;
        self.final_frame = final_frame;
        if self.data_frame {
            match opcode {
                0 if self.fragmented => {}
                1 | 2 if !self.fragmented => {}
                _ => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "无效 WebSocket 分片顺序",
                    ));
                }
            }
            let total = self
                .message_bytes
                .checked_add(length)
                .filter(|total| *total <= MAX_REQUEST_BYTES)
                .ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidData, "WebSocket 请求正文超过上限")
                })?;
            if total > UNBUDGETED_CONTROL_MESSAGE_BYTES {
                let needed = total
                    .saturating_mul(REQUEST_MEMORY_BUDGET_MULTIPLIER)
                    .div_ceil(REQUEST_BODY_BUDGET_UNIT_BYTES);
                let held = self
                    .permit
                    .as_ref()
                    .map_or(0, OwnedSemaphorePermit::num_permits);
                if needed > held {
                    // Never wait while holding a partial fragmented-message reservation.
                    let added = Arc::clone(&self.budget)
                        .try_acquire_many_owned((needed - held) as u32)
                        .map_err(|_| {
                            io::Error::new(
                                io::ErrorKind::OutOfMemory,
                                "router_memory_busy: WebSocket 请求缓冲区已满",
                            )
                        })?;
                    if let Some(permit) = self.permit.as_mut() {
                        permit.merge(added);
                    } else {
                        self.permit = Some(added);
                    }
                }
            }
            self.message_bytes = total;
            self.largest_frame = self.largest_frame.max(length);
            self.fragmented = !final_frame;
        } else if !matches!(opcode, 8..=10) || !final_frame || length > 125 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "无效 WebSocket 控制帧",
            ));
        }
        self.payload_remaining = length;
        Ok(())
    }

    fn frame_consumed(&mut self) {
        if self.data_frame && self.final_frame {
            self.message_complete = true;
        } else if !self.fragmented {
            self.reading = false;
        }
        self.header_read = 0;
        self.header_sent = 0;
        self.header_len = 2;
    }

    fn finish_message(&mut self) -> (Option<OwnedSemaphorePermit>, bool) {
        debug_assert!(self.message_complete);
        let replace_decoder = self.largest_frame > WEBSOCKET_READ_BUFFER_BYTES;
        self.message_complete = false;
        self.message_bytes = 0;
        self.largest_frame = 0;
        self.reading = false;
        (self.permit.take(), replace_decoder)
    }

    fn poll_bytes(
        &mut self,
        cx: &mut TaskContext<'_>,
        target: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        // Coalesce small headers/messages in fixed storage. Large payloads read
        // directly into the decoder, avoiding a second copy on long requests.
        if self.prefetched_start == self.prefetched_end
            && target.remaining() < WEBSOCKET_READ_BUFFER_BYTES
        {
            let Some(stream) = self.stream.as_mut() else {
                return Poll::Ready(Err(io::ErrorKind::NotConnected.into()));
            };
            let mut buffer = ReadBuf::new(&mut self.prefetched);
            std::task::ready!(Pin::new(stream).poll_read(cx, &mut buffer))?;
            self.prefetched_start = 0;
            self.prefetched_end = buffer.filled().len();
            if self.prefetched_end == 0 {
                return Poll::Ready(Ok(()));
            }
        }
        if self.prefetched_start < self.prefetched_end {
            let count = target
                .remaining()
                .min(self.prefetched_end - self.prefetched_start);
            target
                .put_slice(&self.prefetched[self.prefetched_start..self.prefetched_start + count]);
            self.prefetched_start += count;
            return Poll::Ready(Ok(()));
        }
        let Some(stream) = self.stream.as_mut() else {
            return Poll::Ready(Err(io::ErrorKind::NotConnected.into()));
        };
        Pin::new(stream).poll_read(cx, target)
    }
}

impl AsyncRead for WebSocketIngress {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        output: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        if output.remaining() == 0 {
            return Poll::Ready(Ok(()));
        }
        if this.handshake {
            if this.prefetched_start == this.prefetched_end {
                let mut buffer = ReadBuf::new(&mut this.prefetched);
                let stream = this.stream.as_mut().ok_or(io::ErrorKind::NotConnected)?;
                std::task::ready!(Pin::new(stream).poll_read(cx, &mut buffer))?;
                this.prefetched_start = 0;
                this.prefetched_end = buffer.filled().len();
            }
            let start = this.prefetched_start;
            let end = this.prefetched_end.min(start + output.remaining());
            while this.prefetched_start < end {
                this.handshake_tail =
                    (this.handshake_tail << 8) | u32::from(this.prefetched[this.prefetched_start]);
                this.prefetched_start += 1;
                this.handshake_bytes += 1;
                if this.handshake_bytes > MAX_HEADER_BYTES {
                    return Poll::Ready(Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "WebSocket 握手头超过上限",
                    )));
                }
                if this.handshake_tail == u32::from_be_bytes(*b"\r\n\r\n") {
                    this.handshake = false;
                    break;
                }
            }
            output.put_slice(&this.prefetched[start..this.prefetched_start]);
            return Poll::Ready(Ok(()));
        }
        this.poll_deadline(cx)?;
        // A consumer must deliver the complete message before requesting another.
        debug_assert!(!this.message_complete);
        while this.header_read < this.header_len {
            let mut bytes = [0; 14];
            let needed = this.header_len - this.header_read;
            let mut buffer = ReadBuf::new(&mut bytes[..needed]);
            std::task::ready!(this.poll_bytes(cx, &mut buffer))?;
            let read = buffer.filled().len();
            if read == 0 {
                return Poll::Ready(Err(io::ErrorKind::UnexpectedEof.into()));
            }
            this.start_read();
            // Register the timer even if the following socket read is pending.
            this.poll_deadline(cx)?;
            this.header[this.header_read..this.header_read + read].copy_from_slice(buffer.filled());
            this.header_read += read;
            if this.header_read == 2 {
                this.header_len =
                    2 + match this.header[1] & 0x7f {
                        126 => 2,
                        127 => 8,
                        _ => 0,
                    } + if this.header[1] & 0x80 != 0 { 4 } else { 0 };
            }
            if this.header_read == this.header_len {
                this.prepare_header()?;
            }
        }
        if this.header_sent < this.header_len {
            let end = this.header_len.min(this.header_sent + output.remaining());
            output.put_slice(&this.header[this.header_sent..end]);
            this.header_sent = end;
            if this.header_sent == this.header_len && this.payload_remaining == 0 {
                this.frame_consumed();
            }
            return Poll::Ready(Ok(()));
        }
        let count = output.remaining().min(this.payload_remaining);
        let mut payload = ReadBuf::new(output.initialize_unfilled_to(count));
        std::task::ready!(this.poll_bytes(cx, &mut payload))?;
        let read = payload.filled().len();
        if read == 0 {
            return Poll::Ready(Err(io::ErrorKind::UnexpectedEof.into()));
        }
        output.advance(read);
        this.payload_remaining -= read;
        if this.payload_remaining == 0 {
            this.frame_consumed();
        }
        Poll::Ready(Ok(()))
    }
}

impl AsyncWrite for WebSocketIngress {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        // 测试在真实写出指定字节后阻塞，避免依赖操作系统的缓冲区大小。
        #[cfg(test)]
        let bytes = match this.write_remaining {
            Some(0) if !bytes.is_empty() => return Poll::Pending,
            Some(remaining) => &bytes[..bytes.len().min(remaining)],
            None => bytes,
        };
        let written = std::task::ready!(
            Pin::new(this.stream.as_mut().ok_or(io::ErrorKind::NotConnected)?)
                .poll_write(cx, bytes)
        )?;
        #[cfg(test)]
        if let Some(remaining) = &mut this.write_remaining {
            *remaining -= written;
        }
        Poll::Ready(Ok(written))
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<io::Result<()>> {
        Pin::new(
            self.get_mut()
                .stream
                .as_mut()
                .ok_or(io::ErrorKind::NotConnected)?,
        )
        .poll_flush(cx)
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<io::Result<()>> {
        Pin::new(
            self.get_mut()
                .stream
                .as_mut()
                .ok_or(io::ErrorKind::NotConnected)?,
        )
        .poll_shutdown(cx)
    }
}

/// Own both the decoder and its reservation, including across cancelled polls.
pub(crate) struct BudgetedWebSocket {
    socket: Option<WebSocketStream<WebSocketIngress>>,
    pending: Option<WebSocketMessage>,
    delivered_permit: Option<OwnedSemaphorePermit>,
    flush_deadline: Option<tokio::time::Instant>,
}

impl BudgetedWebSocket {
    pub(crate) fn new(socket: WebSocketStream<WebSocketIngress>) -> Self {
        Self {
            socket: Some(socket),
            pending: None,
            delivered_permit: None,
            flush_deadline: None,
        }
    }

    pub(crate) fn set_budget(&mut self, budget: Arc<Semaphore>) {
        let ingress = self.socket.as_mut().unwrap().get_mut();
        assert!(ingress.permit.is_none() && !ingress.reading);
        ingress.budget = budget;
    }

    pub(crate) fn take_message_permit(&mut self) -> Option<OwnedSemaphorePermit> {
        self.delivered_permit.take()
    }

    pub(crate) async fn next(
        &mut self,
    ) -> Option<std::result::Result<WebSocketMessage, WebSocketError>> {
        debug_assert!(
            self.delivered_permit.is_none(),
            "consume the message reservation before reading again"
        );
        if self.pending.is_none() {
            match self.socket.as_mut()?.next().await {
                Some(Ok(message @ (WebSocketMessage::Text(_) | WebSocketMessage::Binary(_)))) => {
                    self.pending = Some(message)
                }
                result @ (None | Some(Err(_))) => {
                    self.socket.take();
                    return result;
                }
                result => return result,
            }
        }
        let socket = self.socket.as_mut()?;
        if socket.get_ref().largest_frame > WEBSOCKET_READ_BUFFER_BYTES {
            // Flush tungstenite's pending automatic Pong and all output before
            // resetting. Keep the message in self if select! cancels this await.
            let deadline = *self
                .flush_deadline
                .get_or_insert_with(|| tokio::time::Instant::now() + DOWNSTREAM_WRITE_TIMEOUT);
            let flushed = match tokio::time::timeout_at(deadline, socket.flush()).await {
                Ok(result) => result,
                Err(_) => Err(WebSocketError::Io(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "WebSocket 控制帧写入超时",
                ))),
            };
            if let Err(error) = flushed {
                self.pending.take();
                self.socket.take();
                return Some(Err(error));
            }
        }
        self.flush_deadline = None;
        let (permit, replace_decoder) = socket.get_mut().finish_message();
        if replace_decoder {
            let config = *socket.get_config();
            let budget = Arc::clone(&socket.get_ref().budget);
            let ingress = std::mem::replace(
                socket.get_mut(),
                WebSocketIngress::empty(None, budget, false),
            );
            self.socket.take();
            // from_raw_socket only constructs a context; it does no I/O and
            // completes on its first poll. Prefetched handshake bytes stay in IO.
            self.socket = Some(
                WebSocketStream::from_raw_socket(
                    ingress,
                    tokio_tungstenite::tungstenite::protocol::Role::Server,
                    Some(config),
                )
                .await,
            );
        }
        self.delivered_permit = permit;
        self.pending.take().map(Ok)
    }

    #[allow(
        clippy::result_large_err,
        reason = "直接转发 tungstenite 错误，保留错误匹配方式并避免额外堆分配"
    )]
    pub(crate) async fn send(
        &mut self,
        message: WebSocketMessage,
    ) -> std::result::Result<(), WebSocketError> {
        // send 可能先排入帧、再等待 flush。将 socket 的所有权留在 future 中，
        // 取消或写入失败时直接关闭，避免下一次 flush 发出旧终态或残缺帧。
        let mut socket = self.socket.take().ok_or(WebSocketError::AlreadyClosed)?;
        socket.send(message).await?;
        self.socket = Some(socket);
        Ok(())
    }

    #[allow(
        clippy::result_large_err,
        reason = "直接转发 tungstenite 错误，保留错误匹配方式并避免额外堆分配"
    )]
    pub(crate) async fn flush(&mut self) -> std::result::Result<(), WebSocketError> {
        self.socket
            .as_mut()
            .ok_or(WebSocketError::AlreadyClosed)?
            .flush()
            .await
    }

    #[allow(
        clippy::result_large_err,
        reason = "直接转发 tungstenite 错误，保留错误匹配方式并避免额外堆分配"
    )]
    pub(crate) async fn close(
        &mut self,
        frame: Option<tokio_tungstenite::tungstenite::protocol::CloseFrame>,
    ) -> std::result::Result<(), WebSocketError> {
        self.socket
            .as_mut()
            .ok_or(WebSocketError::AlreadyClosed)?
            .close(frame)
            .await
    }
}

pub(crate) fn downstream_websocket_config() -> WebSocketConfig {
    WebSocketConfig::default()
        .read_buffer_size(WEBSOCKET_READ_BUFFER_BYTES)
        .write_buffer_size(0)
        .max_write_buffer_size(MAX_UPSTREAM_RESPONSE_BYTES)
        .max_message_size(Some(MAX_REQUEST_BYTES))
        .max_frame_size(Some(MAX_REQUEST_BYTES))
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn raw_pair(budget: Arc<Semaphore>) -> (BudgetedWebSocket, TcpStream) {
        raw_pair_with_config(budget, downstream_websocket_config()).await
    }

    async fn raw_pair_with_config(
        budget: Arc<Semaphore>,
        config: WebSocketConfig,
    ) -> (BudgetedWebSocket, TcpStream) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let client = TcpStream::connect(listener.local_addr().unwrap());
        let (client, server) = tokio::join!(client, listener.accept());
        let ingress = WebSocketIngress::empty(Some(server.unwrap().0), budget, false);
        let socket = WebSocketStream::from_raw_socket(
            ingress,
            tokio_tungstenite::tungstenite::protocol::Role::Server,
            Some(config),
        )
        .await;
        (BudgetedWebSocket::new(socket), client.unwrap())
    }

    fn header(opcode: u8, final_frame: bool, length: usize) -> Vec<u8> {
        let mut header = vec![opcode | if final_frame { 0x80 } else { 0 }];
        if length < 126 {
            header.push(0x80 | length as u8);
        } else if length <= u16::MAX as usize {
            header.push(0x80 | 126);
            header.extend_from_slice(&(length as u16).to_be_bytes());
        } else {
            header.push(0x80 | 127);
            header.extend_from_slice(&(length as u64).to_be_bytes());
        }
        // A zero mask makes synthetic payloads readable without another copy.
        header.extend_from_slice(&[0; 4]);
        header
    }

    async fn frame(client: &mut TcpStream, opcode: u8, final_frame: bool, data: &[u8]) {
        client
            .write_all(&header(opcode, final_frame, data.len()))
            .await
            .unwrap();
        client.write_all(data).await.unwrap();
    }

    async fn wait_for_used(budget: &Semaphore, expected: usize) {
        tokio::time::timeout(Duration::from_secs(2), async {
            while budget.available_permits() != expected {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn slow_large_headers_cannot_allocate_before_the_shared_budget() {
        let budget = Arc::new(Semaphore::new(REQUEST_BODY_BUDGET_PERMITS));
        let (mut admitted, mut slow) = raw_pair(Arc::clone(&budget)).await;
        slow.write_all(&header(2, true, MAX_REQUEST_BYTES))
            .await
            .unwrap();
        let receive = tokio::spawn(async move { admitted.next().await });
        wait_for_used(&budget, 0).await;
        // Only the first announced 64 MiB message can reserve its 4x working set.
        // Other sockets fail after 14 header bytes, without receiving payloads.
        for _ in 0..16 {
            let (mut denied, mut sender) = raw_pair(Arc::clone(&budget)).await;
            sender
                .write_all(&header(2, true, MAX_REQUEST_BYTES))
                .await
                .unwrap();
            let error = denied.next().await.unwrap().unwrap_err();
            assert!(
                matches!(error, WebSocketError::Io(ref error) if error.kind() == io::ErrorKind::OutOfMemory)
            );
            assert!(denied.socket.is_none());
            assert_eq!(budget.available_permits(), 0);
        }
        receive.abort();
        assert!(receive.await.unwrap_err().is_cancelled());
        assert_eq!(budget.available_permits(), REQUEST_BODY_BUDGET_PERMITS);
    }

    #[tokio::test]
    async fn fragmented_messages_reserve_cumulative_bytes_and_release_on_exhaustion() {
        let budget = Arc::new(Semaphore::new(4));
        let (mut socket, mut sender) = raw_pair(Arc::clone(&budget)).await;
        frame(&mut sender, 2, false, &vec![b'x'; 32 * 1024]).await;
        frame(&mut sender, 9, true, b"probe").await;
        assert!(matches!(
            socket.next().await,
            Some(Ok(WebSocketMessage::Ping(_)))
        ));
        assert_eq!(budget.available_permits(), 2);
        frame(&mut sender, 0, false, &vec![b'x'; 32 * 1024]).await;
        frame(&mut sender, 9, true, b"probe2").await;
        assert!(matches!(
            socket.next().await,
            Some(Ok(WebSocketMessage::Ping(_)))
        ));
        assert_eq!(budget.available_permits(), 0);
        // The next fragment must fail immediately, not wait holding other permits.
        sender.write_all(&header(0, true, 1)).await.unwrap();
        let error = socket.next().await.unwrap().unwrap_err();
        assert!(
            matches!(error, WebSocketError::Io(ref error) if error.kind() == io::ErrorKind::OutOfMemory)
        );
        assert_eq!(budget.available_permits(), 4);
    }

    #[tokio::test]
    async fn cancelled_poll_retains_partial_message_until_timeout_or_socket_drop() {
        let budget = Arc::new(Semaphore::new(4));
        let (mut socket, mut sender) = raw_pair(Arc::clone(&budget)).await;
        sender.write_all(&header(2, true, 64 * 1024)).await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(20), socket.next())
                .await
                .is_err()
        );
        assert_eq!(budget.available_permits(), 0);
        // Cancelling a select branch must not release a buffer the decoder owns.
        socket
            .socket
            .as_mut()
            .unwrap()
            .get_mut()
            .read_deadline
            .as_mut()
            .unwrap()
            .as_mut()
            .reset(tokio::time::Instant::now());
        let error = socket.next().await.unwrap().unwrap_err();
        assert!(
            matches!(error, WebSocketError::Io(ref error) if error.kind() == io::ErrorKind::TimedOut)
        );
        assert_eq!(budget.available_permits(), 4);
        let (mut socket, mut sender) = raw_pair(Arc::clone(&budget)).await;
        sender.write_all(&header(2, true, 64 * 1024)).await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(20), socket.next())
                .await
                .is_err()
        );
        drop(socket);
        assert_eq!(budget.available_permits(), 4);
    }

    #[tokio::test(start_paused = true)]
    async fn cancelled_decoder_flush_keeps_message_and_permit_until_fixed_deadline() {
        let budget = Arc::new(Semaphore::new(4));
        let mut ingress = WebSocketIngress::empty(None, Arc::clone(&budget), false);
        // 预填完整入站帧，使轮询只等待被阻塞的写入。
        let mut prefetched = header(1, true, 16 * 1024);
        prefetched.resize(prefetched.len() + 16 * 1024, b'x');
        ingress.prefetched_end = prefetched.len();
        ingress.prefetched = prefetched.into_boxed_slice();
        ingress.write_remaining = Some(0);
        let stream = WebSocketStream::from_raw_socket(
            ingress,
            tokio_tungstenite::tungstenite::protocol::Role::Server,
            Some(downstream_websocket_config().write_buffer_size(1024)),
        )
        .await;
        let mut socket = BudgetedWebSocket::new(stream);
        socket
            .socket
            .as_mut()
            .unwrap()
            .feed(WebSocketMessage::Binary(vec![0].into()))
            .await
            .unwrap();
        // 每次轮询后丢弃尚未完成的 next()，无需等待实际时间即可模拟取消。
        assert!(futures_util::poll!(std::pin::pin!(socket.next())).is_pending());
        assert_eq!(socket.pending.as_ref().unwrap().len(), 16 * 1024);
        assert!(socket.socket.is_some());
        assert_eq!(budget.available_permits(), 3);
        let original_deadline = socket.flush_deadline.unwrap();
        tokio::time::advance(DOWNSTREAM_WRITE_TIMEOUT / 2).await;
        assert!(futures_util::poll!(std::pin::pin!(socket.next())).is_pending());
        assert_eq!(socket.pending.as_ref().unwrap().len(), 16 * 1024);
        assert_eq!(budget.available_permits(), 3);
        assert_eq!(socket.flush_deadline, Some(original_deadline));
        tokio::time::advance(original_deadline - tokio::time::Instant::now()).await;
        let error = socket.next().await.unwrap().unwrap_err();
        assert!(
            matches!(error, WebSocketError::Io(ref error) if error.kind() == io::ErrorKind::TimedOut)
        );
        assert!(socket.socket.is_none());
        assert!(socket.pending.is_none());
        assert!(socket.next().await.is_none());
        assert_eq!(budget.available_permits(), 4);
    }

    #[tokio::test]
    async fn idle_sockets_do_not_take_body_budget_or_block_small_parallel_requests() {
        let budget = Arc::new(Semaphore::new(2));
        let mut idle = Vec::new();
        for _ in 0..64 {
            idle.push(raw_pair(Arc::clone(&budget)).await);
        }
        assert_eq!(budget.available_permits(), 2);
        for (socket, sender) in idle.iter_mut().take(2) {
            frame(sender, 1, true, &vec![b'a'; 8 * 1024]).await;
            assert!(matches!(
                socket.next().await,
                Some(Ok(WebSocketMessage::Text(_)))
            ));
        }
        assert_eq!(budget.available_permits(), 0);
        for (socket, _) in idle.iter_mut().take(2) {
            drop(socket.take_message_permit());
        }
        assert_eq!(budget.available_permits(), 2);
    }

    #[tokio::test]
    async fn full_size_messages_keep_64_mib_compatibility_without_idle_reservations() {
        let budget = Arc::new(Semaphore::new(REQUEST_BODY_BUDGET_PERMITS));
        let (mut socket, mut sender) = raw_pair(Arc::clone(&budget)).await;
        let send = tokio::spawn(async move {
            sender
                .write_all(&header(1, true, MAX_REQUEST_BYTES))
                .await
                .unwrap();
            let chunk = [b'x'; 64 * 1024];
            for _ in 0..MAX_REQUEST_BYTES / chunk.len() {
                sender.write_all(&chunk).await.unwrap();
            }
            frame(&mut sender, 1, true, b"next").await;
            sender
        });
        let message = socket.next().await.unwrap().unwrap();
        assert_eq!(message.len(), MAX_REQUEST_BYTES);
        let permit = socket.take_message_permit().unwrap();
        assert_eq!(permit.num_permits(), REQUEST_BODY_BUDGET_PERMITS);
        assert_eq!(socket.socket.as_ref().unwrap().get_ref().largest_frame, 0);
        drop((message, permit));
        assert_eq!(budget.available_permits(), REQUEST_BODY_BUDGET_PERMITS);
        assert_eq!(
            socket.next().await.unwrap().unwrap().into_text().unwrap(),
            "next"
        );
        let _sender = send.await.unwrap();
    }

    #[tokio::test]
    async fn decoder_reset_preserves_fragmented_text_ping_pong_and_next_message() {
        let budget = Arc::new(Semaphore::new(8));
        let (mut socket, mut sender) = raw_pair(Arc::clone(&budget)).await;
        // UTF-8 can straddle frames; validation remains tungstenite's job.
        let mut first = vec![b'x'; 20 * 1024];
        first.push(0xe4);
        frame(&mut sender, 1, false, &first).await;
        frame(&mut sender, 9, true, b"ping").await;
        frame(&mut sender, 0, true, &[0xb8, 0xad]).await;
        frame(&mut sender, 1, true, b"following").await;
        assert!(matches!(
            socket.next().await,
            Some(Ok(WebSocketMessage::Ping(_)))
        ));
        let text = socket.next().await.unwrap().unwrap().into_text().unwrap();
        assert!(text.ends_with('中'));
        let mut pong = [0; 6];
        sender.read_exact(&mut pong).await.unwrap();
        assert_eq!(&pong, b"\x8a\x04ping");
        drop((text, socket.take_message_permit()));
        assert_eq!(
            socket.next().await.unwrap().unwrap().into_text().unwrap(),
            "following"
        );
        assert_eq!(budget.available_permits(), 8);
        frame(&mut sender, 8, true, &1000u16.to_be_bytes()).await;
        assert!(matches!(
            socket.next().await,
            Some(Ok(WebSocketMessage::Close(_)))
        ));
        socket.flush().await.unwrap();
        let mut close = [0; 4];
        sender.read_exact(&mut close).await.unwrap();
        assert_eq!(&close, b"\x88\x02\x03\xe8");
    }

    #[tokio::test]
    async fn handshake_and_first_frames_in_one_packet_preserve_prefetched_bytes() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let budget = Arc::new(Semaphore::new(8));
        let server_budget = Arc::clone(&budget);
        let address = listener.local_addr().unwrap();
        let accept = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            BudgetedWebSocket::new(
                tokio_tungstenite::accept_async_with_config(
                    WebSocketIngress::new(stream, server_budget),
                    Some(downstream_websocket_config()),
                )
                .await
                .unwrap(),
            )
        });
        let mut sender = TcpStream::connect(address).await.unwrap();
        let mut bytes = b"GET /responses HTTP/1.1\r\nHost: localhost\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n".to_vec();
        bytes.extend_from_slice(&header(1, true, 20 * 1024));
        bytes.extend_from_slice(&vec![b'a'; 20 * 1024]);
        bytes.extend_from_slice(&header(1, true, 4));
        bytes.extend_from_slice(b"next");
        sender.write_all(&bytes).await.unwrap();
        let mut socket = accept.await.unwrap();
        assert_eq!(socket.next().await.unwrap().unwrap().len(), 20 * 1024);
        drop(socket.take_message_permit());
        assert_eq!(
            socket.next().await.unwrap().unwrap().into_text().unwrap(),
            "next"
        );
        assert_eq!(budget.available_permits(), 8);
    }

    #[tokio::test]
    async fn fragmented_size_and_protocol_validation_happen_before_payload_reservation() {
        let budget = Arc::new(Semaphore::new(REQUEST_BODY_BUDGET_PERMITS));
        let mut ingress = WebSocketIngress::empty(None, Arc::clone(&budget), false);
        ingress.fragmented = true;
        ingress.message_bytes = MAX_REQUEST_BYTES - 1;
        let bytes = header(0, true, 2);
        ingress.header[..bytes.len()].copy_from_slice(&bytes);
        assert_eq!(
            ingress.prepare_header().unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        assert_eq!(budget.available_permits(), REQUEST_BODY_BUDGET_PERMITS);
        for bytes in [
            header(0, true, 8),
            header(9, false, 8),
            header(9, true, 126),
            header(3, true, 8),
        ] {
            let mut ingress = WebSocketIngress::empty(None, Arc::clone(&budget), false);
            ingress.header[..bytes.len()].copy_from_slice(&bytes);
            assert!(ingress.prepare_header().is_err());
            assert!(ingress.permit.is_none());
        }
    }

    async fn assert_send_connection_closed(socket: &mut BudgetedWebSocket) {
        tokio::time::timeout(Duration::from_secs(2), async {
            assert!(matches!(
                socket.send(WebSocketMessage::Text("later".into())).await,
                Err(WebSocketError::AlreadyClosed)
            ));
            assert!(matches!(
                socket.flush().await,
                Err(WebSocketError::AlreadyClosed)
            ));
            assert!(socket.next().await.is_none());
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn plain_websocket_send_success_keeps_connection_reusable() {
        let budget = Arc::new(Semaphore::new(8));
        let (mut socket, peer) = raw_pair(budget).await;
        let mut peer = WebSocketStream::from_raw_socket(
            peer,
            tokio_tungstenite::tungstenite::protocol::Role::Client,
            None,
        )
        .await;
        tokio::time::timeout(Duration::from_secs(2), async {
            for (response, request) in [("reply-1", "request-1"), ("reply-2", "request-2")] {
                socket
                    .send(WebSocketMessage::Text(response.into()))
                    .await
                    .unwrap();
                socket.flush().await.unwrap();
                assert_eq!(
                    peer.next().await.unwrap().unwrap().into_text().unwrap(),
                    response
                );
                peer.send(WebSocketMessage::Text(request.into()))
                    .await
                    .unwrap();
                assert_eq!(
                    socket.next().await.unwrap().unwrap().into_text().unwrap(),
                    request
                );
                drop(socket.take_message_permit());
            }
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn plain_websocket_send_cancellation_closes_connection() {
        let budget = Arc::new(Semaphore::new(8));
        let (mut socket, mut peer) = raw_pair(budget).await;
        socket.socket.as_mut().unwrap().get_mut().write_remaining = Some(128);
        let payload_bytes = 8 * 1024;
        assert!(
            tokio::time::timeout(
                Duration::from_millis(100),
                socket.send(WebSocketMessage::Text("x".repeat(payload_bytes).into())),
            )
            .await
            .is_err()
        );
        assert_send_connection_closed(&mut socket).await;
        let mut bytes = Vec::new();
        tokio::time::timeout(Duration::from_secs(3), peer.read_to_end(&mut bytes))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(bytes.len(), 128, "取消发生前应已写出指定长度的部分帧");
        assert!(bytes.len() < payload_bytes, "取消后不得补发完整帧");
    }

    #[tokio::test]
    async fn plain_websocket_send_error_closes_connection() {
        let budget = Arc::new(Semaphore::new(8));
        let (mut socket, peer) = raw_pair(budget).await;
        // 零等待关闭会发送 TCP reset，验证真实传输错误而非注入的写入错误。
        socket2::SockRef::from(&peer)
            .set_linger(Some(Duration::ZERO))
            .unwrap();
        drop(peer);
        let ingress = socket.socket.as_ref().unwrap().get_ref();
        tokio::time::timeout(
            Duration::from_secs(2),
            ingress.stream.as_ref().unwrap().readable(),
        )
        .await
        .unwrap()
        .unwrap();
        let result = tokio::time::timeout(
            Duration::from_secs(2),
            socket.send(WebSocketMessage::Text("after reset".into())),
        )
        .await
        .unwrap();
        assert!(matches!(result, Err(WebSocketError::Io(_))));
        assert_send_connection_closed(&mut socket).await;
    }

    #[tokio::test]
    async fn cancelled_terminal_write_discards_buffer_and_does_not_remember_native_history() {
        let budget = Arc::new(Semaphore::new(REQUEST_BODY_BUDGET_PERMITS));
        let (mut socket, mut peer) = raw_pair(budget).await;
        socket.socket.as_mut().unwrap().get_mut().write_remaining = Some(128);
        let mut downstream = WebSocketResponsesDownstream::new(socket);
        let (config, provider, model) =
            super::super::tests::router_config("http://127.0.0.1:9/v1".into());
        let snapshot = RouterSnapshot::from_config(&config);
        downstream.prepare_native_http_fallback(
            &snapshot.routes[&provider], &HeaderMap::new(),
            &mut json!({"model":model,"input":[{"role":"user","content":"context"},{"type":"compaction_trigger"}]}),
        ).unwrap();
        let event = json!({"type":"response.completed","response":{
            "id":"resp-not-delivered","object":"response","status":"completed",
            "output":[{"type":"compaction","encrypted_content":"x".repeat(8 * 1024)}]
        }});
        assert!(
            tokio::time::timeout(Duration::from_millis(200), downstream.write_event(&event))
                .await
                .is_err()
        );
        assert!(downstream.terminal_started);
        assert!(!downstream.native_history.has_history());
        downstream.finish_compaction_interrupt().await.unwrap();
        assert!(downstream.socket.flush().await.is_err());
        assert!(!downstream.native_history.has_history());
        let mut bytes = Vec::new();
        tokio::time::timeout(Duration::from_secs(3), peer.read_to_end(&mut bytes))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(bytes.len(), 128, "取消发生前应已写出指定长度的部分帧");
        assert!(
            bytes.len() < event.to_string().len(),
            "cancelled frame must remain incomplete"
        );
    }
}
