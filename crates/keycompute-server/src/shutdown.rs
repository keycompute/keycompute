//! Stop new work first, retain authenticated Node completion/heartbeat ingress,
//! then stop TCP acceptance after admitted work drains. A deadline cancels IO.
use crate::{ApiError, AppState};
use axum::{
    Router,
    body::Body,
    extract::{Request, State},
    http::{HeaderValue, Method, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use serde::Serialize;
use std::{
    future::{Future, IntoFuture},
    io,
    net::SocketAddr,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    task::{Context, Poll},
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    net::{TcpListener, TcpStream},
    sync::Notify,
};
type Signal = Pin<Box<dyn Future<Output = ()> + Send>>;
#[derive(Debug, Default)]
pub struct ShutdownState {
    draining: AtomicBool,
    forced: AtomicBool,
    http: AtomicUsize,
    sockets: AtomicUsize,
    transports: AtomicUsize,
    drain_signal: Notify,
    force_signal: Notify,
}
#[derive(Debug, Serialize)]
pub struct ShutdownSnapshot {
    pub draining: bool,
    pub forced: bool,
    pub http_inflight: usize,
    pub websockets: usize,
    pub open_transports: usize,
}
#[derive(Clone, Copy)]
enum WorkKind {
    Http,
    WebSocket,
    Transport,
}
pub(crate) struct WorkPermit {
    owner: Arc<ShutdownState>,
    kind: WorkKind,
}
impl Drop for WorkPermit {
    fn drop(&mut self) {
        self.owner.counter(self.kind).fetch_sub(1, Ordering::SeqCst);
    }
}
impl ShutdownState {
    fn counter(&self, kind: WorkKind) -> &AtomicUsize {
        match kind {
            WorkKind::Http => &self.http,
            WorkKind::WebSocket => &self.sockets,
            WorkKind::Transport => &self.transports,
        }
    }
    fn enter(self: &Arc<Self>, kind: WorkKind, allow_drain: bool) -> Option<WorkPermit> {
        if !allow_drain && self.is_draining() {
            return None;
        }
        self.counter(kind).fetch_add(1, Ordering::SeqCst);
        let permit = WorkPermit {
            owner: self.clone(),
            kind,
        };
        // Recheck a registrar that raced with the draining flag.
        if !allow_drain && self.is_draining() {
            return None;
        }
        Some(permit)
    }
    pub(crate) fn begin(&self) {
        self.draining.store(true, Ordering::SeqCst);
        self.drain_signal.notify_waiters();
    }
    pub(crate) fn force(&self) {
        self.forced.store(true, Ordering::SeqCst);
        self.force_signal.notify_waiters();
    }
    pub fn is_draining(&self) -> bool {
        self.draining.load(Ordering::SeqCst)
    }
    pub fn is_forced(&self) -> bool {
        self.forced.load(Ordering::SeqCst)
    }
    pub fn snapshot(&self) -> ShutdownSnapshot {
        ShutdownSnapshot {
            draining: self.is_draining(),
            forced: self.is_forced(),
            http_inflight: self.http.load(Ordering::SeqCst),
            websockets: self.sockets.load(Ordering::SeqCst),
            open_transports: self.transports.load(Ordering::SeqCst),
        }
    }
    pub async fn draining(&self) {
        loop {
            let wake = self.drain_signal.notified();
            tokio::pin!(wake);
            wake.as_mut().enable();
            if self.is_draining() {
                return;
            }
            wake.await;
        }
    }
    pub async fn forced(&self) {
        loop {
            let wake = self.force_signal.notified();
            tokio::pin!(wake);
            wake.as_mut().enable();
            if self.is_forced() {
                return;
            }
            wake.await;
        }
    }
    pub(crate) fn socket(self: &Arc<Self>) -> Option<WorkPermit> {
        self.enter(WorkKind::WebSocket, false)
    }
    fn force_future(self: &Arc<Self>) -> Signal {
        let state = self.clone();
        Box::pin(async move { state.forced().await })
    }
}
fn probe_path(path: &str) -> bool {
    matches!(path, "/health" | "/ready")
}
fn completion_during_drain(method: &Method, path: &str) -> bool {
    // Keep the existing authenticated diagnostics usable during a drain.
    if method == Method::GET && path == "/api/v1/admin/monitoring/capacity" {
        return true;
    }
    if method != Method::POST {
        return false;
    }
    if path == "/node/v1/heartbeat" {
        return true;
    }
    path.strip_prefix("/node/v1/tasks/")
        .and_then(|s| s.strip_suffix("/complete"))
        .is_some_and(|id| uuid::Uuid::parse_str(id).is_ok())
}
pub(crate) fn unavailable() -> Response {
    let mut response =
        ApiError::ServiceUnavailable("Server is draining; retry on an available instance".into())
            .into_response();
    response
        .headers_mut()
        .insert("retry-after", HeaderValue::from_static("1"));
    response
}
pub async fn readiness(State(state): State<AppState>) -> Response {
    if state.shutdown.is_draining() {
        (StatusCode::SERVICE_UNAVAILABLE, "draining").into_response()
    } else {
        (StatusCode::OK, "ready").into_response()
    }
}
pub async fn middleware(State(state): State<AppState>, request: Request, next: Next) -> Response {
    if probe_path(request.uri().path()) {
        return next.run(request).await;
    }
    let Some(permit) = state.shutdown.enter(
        WorkKind::Http,
        completion_during_drain(request.method(), request.uri().path()),
    ) else {
        return unavailable();
    };
    let response = tokio::select! {biased;
        _=state.shutdown.forced()=>return unavailable(),
        response=next.run(request)=>response,
    };
    let (parts, body) = response.into_parts();
    Response::from_parts(
        parts,
        Body::new(DrainBody {
            inner: body,
            permit: Some(permit),
            state: state.shutdown.clone(),
            signal: state.shutdown.force_future(),
            ended: false,
        }),
    )
}
struct DrainBody {
    inner: Body,
    permit: Option<WorkPermit>,
    state: Arc<ShutdownState>,
    signal: Signal,
    ended: bool,
}
impl http_body::Body for DrainBody {
    type Data = bytes::Bytes;
    type Error = io::Error;
    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<http_body::Frame<Self::Data>, Self::Error>>> {
        if self.ended {
            return Poll::Ready(None);
        }
        if self.state.is_forced() || self.signal.as_mut().poll(cx).is_ready() {
            self.inner = Body::empty();
            self.permit = None;
            self.ended = true;
            return Poll::Ready(Some(Err(io::Error::new(
                io::ErrorKind::ConnectionAborted,
                "server drain deadline reached",
            ))));
        }
        match Pin::new(&mut self.inner).poll_frame(cx) {
            Poll::Ready(None) => {
                self.permit = None;
                self.ended = true;
                Poll::Ready(None)
            }
            Poll::Ready(Some(Err(error))) => {
                self.permit = None;
                self.ended = true;
                Poll::Ready(Some(Err(io::Error::other(error))))
            }
            Poll::Ready(Some(Ok(frame))) => Poll::Ready(Some(Ok(frame))),
            Poll::Pending => Poll::Pending,
        }
    }
    fn is_end_stream(&self) -> bool {
        self.ended || self.inner.is_end_stream()
    }
    fn size_hint(&self) -> http_body::SizeHint {
        self.inner.size_hint()
    }
}
// Dropping Serve does not abort Axum's spawned drivers. Accepted transports
// therefore carry an independent force signal, including WebSocket upgrades.
struct DrainListener {
    inner: TcpListener,
    state: Arc<ShutdownState>,
}
impl axum::serve::Listener for DrainListener {
    type Io = DrainIo;
    type Addr = SocketAddr;
    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        let (inner, address) = axum::serve::Listener::accept(&mut self.inner).await;
        let permit = self
            .state
            .enter(WorkKind::Transport, true)
            .expect("transport tracking is never refused");
        (
            DrainIo {
                inner,
                state: self.state.clone(),
                read_signal: self.state.force_future(),
                write_signal: self.state.force_future(),
                _permit: permit,
            },
            address,
        )
    }
    fn local_addr(&self) -> io::Result<SocketAddr> {
        self.inner.local_addr()
    }
}
struct DrainIo {
    inner: TcpStream,
    state: Arc<ShutdownState>,
    read_signal: Signal,
    write_signal: Signal,
    _permit: WorkPermit,
}
impl DrainIo {
    fn read_stopped(&mut self, cx: &mut Context<'_>) -> bool {
        self.state.is_forced() || self.read_signal.as_mut().poll(cx).is_ready()
    }
    fn write_stopped(&mut self, cx: &mut Context<'_>) -> bool {
        self.state.is_forced() || self.write_signal.as_mut().poll(cx).is_ready()
    }
    fn stopped() -> io::Error {
        io::Error::new(
            io::ErrorKind::ConnectionAborted,
            "server drain deadline reached",
        )
    }
}
impl AsyncRead for DrainIo {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if self.read_stopped(cx) {
            return Poll::Ready(Err(Self::stopped()));
        }
        Pin::new(&mut self.inner).poll_read(cx, buffer)
    }
}
impl AsyncWrite for DrainIo {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<io::Result<usize>> {
        if self.write_stopped(cx) {
            return Poll::Ready(Err(Self::stopped()));
        }
        Pin::new(&mut self.inner).poll_write(cx, buffer)
    }
    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffers: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        if self.write_stopped(cx) {
            return Poll::Ready(Err(Self::stopped()));
        }
        Pin::new(&mut self.inner).poll_write_vectored(cx, buffers)
    }
    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored()
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        if self.write_stopped(cx) {
            return Poll::Ready(Err(Self::stopped()));
        }
        Pin::new(&mut self.inner).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}
fn application_drained(state: &AppState) -> bool {
    let http = state.shutdown.snapshot();
    let requests = state.generation_admission.requests.status();
    let ingress = state.generation_admission.ingress.status();
    let accounts = state.generation_admission.accounts.status();
    let balance = keycompute_billing::balance::BalanceService::request_reservation_status();
    http.http_inflight == 0
        && http.websockets == 0
        && requests.active == 0
        && requests.queued == 0
        && ingress.active == 0
        && ingress.queued == 0
        && accounts.active == 0
        && accounts.queued == 0
        && balance.active == 0
        && balance.queued == 0
}
// Cancelled serving futures must close their separately spawned IO drivers.
struct ServingGuard {
    state: AppState,
    completed: bool,
}
impl Drop for ServingGuard {
    fn drop(&mut self) {
        if !self.completed {
            self.state.begin_draining();
            self.state.shutdown.force();
        }
    }
}

pub(crate) async fn serve_router_with_shutdown(
    listener: TcpListener,
    router: Router,
    state: AppState,
    shutdown: impl Future<Output = ()> + Send,
    budget: Duration,
) -> crate::Result<()> {
    if budget.is_zero() || budget > Duration::from_secs(3600) {
        return Err(ApiError::Config("invalid graceful drain budget".into()));
    }
    let mut guard = ServingGuard {
        state: state.clone(),
        completed: false,
    };
    let (close, closed) = tokio::sync::oneshot::channel::<()>();
    let mut close = Some(close);
    let server = axum::serve(
        DrainListener {
            inner: listener,
            state: state.shutdown.clone(),
        },
        router,
    )
    .with_graceful_shutdown(async {
        let _ = closed.await;
    })
    .into_future();
    tokio::pin!(server);
    tokio::pin!(shutdown);
    tokio::select! {biased;
        result=&mut server=>return result.map_err(|error|ApiError::Internal(format!("Server error: {error}"))),
        _=&mut shutdown=>{},
    }
    state.begin_draining();
    tracing::info!(
        budget_secs = budget.as_secs(),
        "Server draining: rejecting new work; allowing admitted work and Node completions"
    );
    let deadline = tokio::time::Instant::now() + budget;
    let drained = tokio::time::timeout_at(deadline, async {
        while !application_drained(&state) {
            // Keep accepting/polling the server while existing Node work needs
            // completion callbacks on new TCP connections. Sleeping alone here
            // would suspend the Serve accept loop during the very drain window.
            tokio::select! {
                result = &mut server => return result,
                _ = tokio::time::sleep(Duration::from_millis(10)) => {},
            }
        }
        let _ = close.take().expect("listener stop sent once").send(());
        (&mut server).await
    })
    .await;
    match drained {
        Ok(result) => {
            guard.completed = result.is_ok();
            result.map_err(|error| ApiError::Internal(format!("Server drain error: {error}")))
        }
        Err(_) => {
            state.shutdown.force();
            if let Some(close) = close.take() {
                let _ = close.send(());
            }
            // Brief cooperative cleanup is bounded independently. Durable
            // recovery handles unfinished money/usage work after process exit.
            let _ = tokio::time::timeout(Duration::from_secs(1), &mut server).await;
            tracing::warn!(state=?state.shutdown.snapshot(),"Server drain deadline expired; transports cancelled, durable recovery required");
            Err(ApiError::ServiceUnavailable(
                "Server drain deadline expired".into(),
            ))
        }
    }
}

#[cfg(test)]
#[path = "drain_tests.rs"]
mod tests;
