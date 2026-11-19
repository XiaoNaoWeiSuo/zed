use futures::{
    AsyncRead,
    future::{BoxFuture, Either, select},
};
use gpui::{BackgroundExecutor, Task};
use http_client::{AsyncBody, HttpClient, Request, Response, Url, http::HeaderValue};
use std::{
    io,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};

const HEADER_TIMEOUT: Duration = Duration::from_secs(120);
const BODY_IDLE_TIMEOUT: Duration = Duration::from_secs(120);

/// Bounds waiting for LLM response headers and pauses between response bytes without
/// imposing a total duration limit on a live completion stream.
pub fn completion_http_client(
    client: Arc<dyn HttpClient>,
    executor: BackgroundExecutor,
) -> Arc<dyn HttpClient> {
    Arc::new(CompletionHttpClient { client, executor })
}

/// Recognizes transport timeouts, including errors wrapped with provider context.
pub fn is_completion_timeout(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<io::Error>()
            .is_some_and(|error| error.kind() == io::ErrorKind::TimedOut)
    })
}

struct CompletionHttpClient {
    client: Arc<dyn HttpClient>,
    executor: BackgroundExecutor,
}

impl HttpClient for CompletionHttpClient {
    fn user_agent(&self) -> Option<&HeaderValue> {
        self.client.user_agent()
    }

    fn proxy(&self) -> Option<&Url> {
        self.client.proxy()
    }

    fn send(
        &self,
        request: Request<AsyncBody>,
    ) -> BoxFuture<'static, anyhow::Result<Response<AsyncBody>>> {
        let client = self.client.clone();
        let executor = self.executor.clone();
        Box::pin(async move {
            let timeout = executor.timer(HEADER_TIMEOUT);
            let response = match select(client.send(request), timeout).await {
                Either::Left((response, _timeout)) => response?,
                Either::Right(((), _request)) => {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "LLM response headers timed out",
                    )
                    .into());
                }
            };
            Ok(response.map(|body| AsyncBody::from_reader(IdleTimeoutBody::new(body, executor))))
        })
    }
}

struct IdleTimeoutBody {
    body: Option<AsyncBody>,
    executor: BackgroundExecutor,
    timeout: Option<Task<()>>,
    timed_out: bool,
}

impl IdleTimeoutBody {
    fn new(body: AsyncBody, executor: BackgroundExecutor) -> Self {
        let timeout = Some(executor.timer(BODY_IDLE_TIMEOUT));
        Self {
            body: Some(body),
            executor,
            timeout,
            timed_out: false,
        }
    }
}

impl AsyncRead for IdleTimeoutBody {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        if buffer.is_empty() {
            return Poll::Ready(Ok(0));
        }
        if self.timed_out {
            return Poll::Ready(Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "LLM response body idle timeout",
            )));
        }

        let Some(body) = self.body.as_mut() else {
            return Poll::Ready(Ok(0));
        };
        match Pin::new(body).poll_read(cx, buffer) {
            Poll::Ready(Ok(count)) if count > 0 => {
                // Transport progress, including SSE comments, keeps the stream alive.
                self.timeout = Some(self.executor.timer(BODY_IDLE_TIMEOUT));
                Poll::Ready(Ok(count))
            }
            Poll::Ready(result) => {
                self.timeout = None;
                self.body = None;
                Poll::Ready(result)
            }
            Poll::Pending => {
                // A paused consumer can resume with both data and an expired timer ready.
                // Only a pending transport read is evidence of an idle connection.
                if let Some(timeout) = self.timeout.as_mut() {
                    if Pin::new(timeout).poll(cx).is_ready() {
                        self.timed_out = true;
                        self.timeout = None;
                        self.body = None;
                        return Poll::Ready(Err(io::Error::new(
                            io::ErrorKind::TimedOut,
                            "LLM response body idle timeout",
                        )));
                    }
                }
                Poll::Pending
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::{AsyncReadExt, FutureExt, Stream, channel::mpsc};
    use gpui::TestAppContext;
    use http_client::{FakeHttpClient, HttpClientWithProxy};
    use std::sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    };

    struct TestBody {
        receiver: Mutex<mpsc::UnboundedReceiver<u8>>,
        dropped: Arc<AtomicBool>,
    }

    impl AsyncRead for TestBody {
        fn poll_read(
            self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buffer: &mut [u8],
        ) -> Poll<io::Result<usize>> {
            if buffer.is_empty() {
                return Poll::Ready(Ok(0));
            }
            match Pin::new(&mut *self.receiver.lock().expect("receiver lock")).poll_next(cx) {
                Poll::Ready(Some(byte)) => {
                    buffer[0] = byte;
                    Poll::Ready(Ok(1))
                }
                Poll::Ready(None) => Poll::Ready(Ok(0)),
                Poll::Pending => Poll::Pending,
            }
        }
    }

    impl Drop for TestBody {
        fn drop(&mut self) {
            self.dropped.store(true, Ordering::SeqCst);
        }
    }

    fn test_body() -> (AsyncBody, mpsc::UnboundedSender<u8>, Arc<AtomicBool>) {
        let (sender, receiver) = mpsc::unbounded();
        let dropped = Arc::new(AtomicBool::new(false));
        let body = AsyncBody::from_reader(TestBody {
            receiver: Mutex::new(receiver),
            dropped: dropped.clone(),
        });
        (body, sender, dropped)
    }

    async fn open_body(
        executor: BackgroundExecutor,
    ) -> (AsyncBody, mpsc::UnboundedSender<u8>, Arc<AtomicBool>) {
        let (body, sender, dropped) = test_body();
        let body = Mutex::new(Some(body));
        let client = FakeHttpClient::create(move |_| {
            let body = body.lock().expect("body lock").take().expect("one request");
            async move { Ok(Response::new(body)) }
        });
        let response = completion_http_client(client, executor)
            .get("http://test.example/completion", AsyncBody::empty(), false)
            .await
            .expect("response headers");
        (response.into_body(), sender, dropped)
    }

    fn pending_client(executor: BackgroundExecutor) -> Arc<dyn HttpClient> {
        let client = FakeHttpClient::create(|request| async move {
            let response = futures::future::pending().await;
            drop(request);
            response
        });
        completion_http_client(client, executor)
    }

    #[gpui::test]
    async fn header_timeout_drops_pending_request(cx: &mut TestAppContext) {
        let (body, _sender, dropped) = test_body();
        let client = pending_client(cx.background_executor.clone());
        let mut request = client.get("http://test.example/completion", body, false);
        assert!((&mut request).now_or_never().is_none());
        cx.background_executor
            .advance_clock(HEADER_TIMEOUT - Duration::from_secs(1));
        assert!((&mut request).now_or_never().is_none());
        assert!(!dropped.load(Ordering::SeqCst));
        cx.background_executor.advance_clock(Duration::from_secs(1));
        let error = request.await.err().expect("header timeout");
        assert!(is_completion_timeout(&error.context("provider context")));
        assert!(dropped.load(Ordering::SeqCst));
    }

    #[gpui::test]
    async fn body_idle_timeout_wakes_reader_and_drops_transport(cx: &mut TestAppContext) {
        let (mut body, _sender, dropped) = open_body(cx.background_executor.clone()).await;
        let read = cx.background_executor.spawn(async move {
            let mut buffer = [0];
            let result = body.read(&mut buffer).await;
            (body, result)
        });
        cx.run_until_parked();
        cx.background_executor.advance_clock(BODY_IDLE_TIMEOUT);
        cx.run_until_parked();
        assert!(dropped.load(Ordering::SeqCst));
        let (mut body, result) = read.now_or_never().expect("timer woke reader");
        let error = result.expect_err("idle timeout");
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(is_completion_timeout(&error.into()));
        assert_eq!(
            body.read(&mut [0])
                .await
                .expect_err("persistent timeout")
                .kind(),
            io::ErrorKind::TimedOut
        );
    }

    #[gpui::test]
    async fn keepalive_bytes_extend_stream_beyond_total_timeout(cx: &mut TestAppContext) {
        let (mut body, sender, dropped) = open_body(cx.background_executor.clone()).await;
        let mut buffer = [0];
        for _ in 0..4 {
            cx.background_executor
                .advance_clock(Duration::from_secs(60));
            sender.unbounded_send(b':').expect("keepalive");
            assert_eq!(body.read(&mut buffer).await.expect("live stream"), 1);
            assert_eq!(buffer, [b':']);
        }
        assert!(!dropped.load(Ordering::SeqCst));
        assert!(body.read(&mut buffer).now_or_never().is_none());
        cx.background_executor.advance_clock(BODY_IDLE_TIMEOUT);
        assert_eq!(
            body.read(&mut buffer)
                .await
                .expect_err("idle after keepalives")
                .kind(),
            io::ErrorKind::TimedOut
        );
        assert!(dropped.load(Ordering::SeqCst));
    }

    #[gpui::test]
    async fn ready_data_wins_over_expired_idle_timer(cx: &mut TestAppContext) {
        let (mut body, sender, dropped) = open_body(cx.background_executor.clone()).await;
        let mut buffer = [0];
        assert!(body.read(&mut buffer).now_or_never().is_none());
        sender.unbounded_send(b':').expect("buffered keepalive");
        cx.background_executor
            .advance_clock(BODY_IDLE_TIMEOUT + Duration::from_secs(1));
        cx.run_until_parked();
        assert_eq!(body.read(&mut buffer).await.expect("buffered data wins"), 1);
        assert!(!dropped.load(Ordering::SeqCst));
        assert!(body.read(&mut buffer).now_or_never().is_none());
        cx.background_executor
            .advance_clock(BODY_IDLE_TIMEOUT - Duration::from_secs(1));
        assert!(body.read(&mut buffer).now_or_never().is_none());
        cx.background_executor.advance_clock(Duration::from_secs(1));
        assert_eq!(
            body.read(&mut buffer)
                .await
                .expect_err("reset idle timeout")
                .kind(),
            io::ErrorKind::TimedOut
        );
    }

    #[gpui::test]
    async fn empty_reads_do_not_cancel_idle_timeout(cx: &mut TestAppContext) {
        let (mut body, _sender, dropped) = open_body(cx.background_executor.clone()).await;
        cx.background_executor
            .advance_clock(Duration::from_secs(60));
        assert_eq!(body.read(&mut []).await.expect("empty read"), 0);
        cx.background_executor
            .advance_clock(Duration::from_secs(60));
        let mut buffer = [0];
        assert_eq!(
            body.read(&mut buffer)
                .await
                .expect_err("original idle deadline")
                .kind(),
            io::ErrorKind::TimedOut
        );
        assert!(dropped.load(Ordering::SeqCst));
    }

    #[gpui::test]
    async fn eof_cleans_up_transport_and_remains_eof(cx: &mut TestAppContext) {
        let (mut body, sender, dropped) = open_body(cx.background_executor.clone()).await;
        drop(sender);
        let mut buffer = [0];
        assert_eq!(body.read(&mut buffer).await.expect("EOF"), 0);
        assert!(dropped.load(Ordering::SeqCst));
        cx.background_executor.advance_clock(BODY_IDLE_TIMEOUT);
        assert_eq!(body.read(&mut buffer).await.expect("EOF after deadline"), 0);
    }

    #[gpui::test]
    async fn cancellation_drops_request_and_response_body(cx: &mut TestAppContext) {
        let (body, _sender, dropped) = test_body();
        let client = pending_client(cx.background_executor.clone());
        let mut request = client.get("http://test.example/completion", body, false);
        assert!((&mut request).now_or_never().is_none());
        drop(request);
        assert!(dropped.load(Ordering::SeqCst));

        let (mut body, _sender, dropped) = open_body(cx.background_executor.clone()).await;
        let mut buffer = [0];
        assert!(body.read(&mut buffer).now_or_never().is_none());
        drop(body);
        assert!(dropped.load(Ordering::SeqCst));
        cx.background_executor.advance_clock(BODY_IDLE_TIMEOUT);
    }

    #[gpui::test]
    fn preserves_user_agent_and_proxy(cx: &mut TestAppContext) {
        let proxy = Url::parse("http://proxy.example:8080").expect("proxy URL");
        let client: Arc<dyn HttpClient> = Arc::new(HttpClientWithProxy::new_url(
            FakeHttpClient::with_200_response(),
            Some(proxy.clone()),
        ));
        let wrapped = completion_http_client(client.clone(), cx.background_executor.clone());
        assert!(client.user_agent().is_some());
        assert_eq!(wrapped.user_agent(), client.user_agent());
        assert_eq!(wrapped.proxy(), Some(&proxy));
        assert!(!is_completion_timeout(&anyhow::anyhow!(
            "ordinary provider error"
        )));
        assert!(!is_completion_timeout(
            &io::Error::new(io::ErrorKind::ConnectionReset, "reset").into()
        ));
    }
}
