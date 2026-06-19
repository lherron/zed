use std::{
    fs,
    io::{ErrorKind, Read, Write},
    os::unix::{
        fs::PermissionsExt as _,
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc::{self, SyncSender, TrySendError},
    },
    thread,
};

use anyhow::{Context as _, Result, anyhow};
use futures::channel::mpsc as futures_mpsc;

use crate::{
    framing::{FrameDecoder, MAX_INBOUND_QUEUED, MAX_OUTBOUND_QUEUED, encode_frame},
    protocol::{Request, Response},
};

pub struct RequestEnvelope {
    pub request: Request,
    pub responder: Responder,
    _permit: InboundPermit,
}

#[derive(Clone)]
pub struct Responder {
    outbound_tx: SyncSender<Response>,
    close_handle: CloseHandle,
}

impl Responder {
    pub fn respond(&self, response: Option<Response>) {
        let Some(response) = response else {
            return;
        };

        match self.outbound_tx.try_send(response) {
            Ok(()) => {}
            Err(TrySendError::Disconnected(_)) | Err(TrySendError::Full(_)) => {
                self.close_handle.close();
            }
        }
    }
}

#[derive(Clone)]
struct CloseHandle {
    stream: Arc<Mutex<Option<UnixStream>>>,
    closed: Arc<AtomicBool>,
}

impl CloseHandle {
    fn new(stream: UnixStream) -> Self {
        Self {
            stream: Arc::new(Mutex::new(Some(stream))),
            closed: Arc::new(AtomicBool::new(false)),
        }
    }

    fn close(&self) {
        if self.closed.swap(true, Ordering::AcqRel) {
            return;
        }

        if let Ok(mut stream) = self.stream.lock()
            && let Some(stream) = stream.take()
        {
            let _ = stream.shutdown(std::net::Shutdown::Both);
        }
    }
}

struct InboundPermit(Arc<AtomicUsize>);

impl Drop for InboundPermit {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

pub fn start(path: PathBuf) -> Result<futures_mpsc::UnboundedReceiver<RequestEnvelope>> {
    prepare_socket_path(&path)?;
    let listener = UnixListener::bind(&path)
        .with_context(|| format!("binding Buffer RPC socket {}", path.display()))?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
        .with_context(|| format!("setting Buffer RPC socket permissions {}", path.display()))?;

    let (requests_tx, requests_rx) = futures_mpsc::unbounded();
    thread::Builder::new()
        .name("buffer-rpc-listener".to_string())
        .spawn(move || accept_loop(listener, requests_tx))
        .context("spawning Buffer RPC listener thread")?;

    Ok(requests_rx)
}

fn prepare_socket_path(path: &Path) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("socket path has no parent: {}", path.display()))?;
    fs::create_dir_all(parent)
        .with_context(|| format!("creating Buffer RPC socket dir {}", parent.display()))?;

    match UnixStream::connect(path) {
        Ok(_) => Err(anyhow!(
            "Buffer RPC socket path is already in use: {}",
            path.display()
        )),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) if error.kind() == ErrorKind::ConnectionRefused => fs::remove_file(path)
            .with_context(|| format!("removing stale Buffer RPC socket {}", path.display())),
        Err(error) => {
            Err(error).with_context(|| format!("checking Buffer RPC socket {}", path.display()))
        }
    }
}

fn accept_loop(
    listener: UnixListener,
    requests_tx: futures_mpsc::UnboundedSender<RequestEnvelope>,
) {
    for stream in listener.incoming() {
        let Ok(stream) = stream else {
            break;
        };

        let requests_tx = requests_tx.clone();
        let _ = thread::Builder::new()
            .name("buffer-rpc-connection".to_string())
            .spawn(move || handle_connection(stream, requests_tx));
    }
}

fn handle_connection(
    stream: UnixStream,
    requests_tx: futures_mpsc::UnboundedSender<RequestEnvelope>,
) {
    let Ok(writer_stream) = stream.try_clone() else {
        return;
    };
    let Ok(close_stream) = stream.try_clone() else {
        return;
    };
    let (outbound_tx, outbound_rx) = mpsc::sync_channel(MAX_OUTBOUND_QUEUED);
    let close_handle = CloseHandle::new(close_stream);
    let writer = thread::Builder::new()
        .name("buffer-rpc-writer".to_string())
        .spawn(move || write_loop(writer_stream, outbound_rx));

    read_loop(
        stream,
        requests_tx,
        Responder {
            outbound_tx,
            close_handle: close_handle.clone(),
        },
        Arc::new(AtomicUsize::new(0)),
    );
    close_handle.close();

    if let Ok(writer) = writer {
        let _ = writer.join();
    }
}

fn read_loop(
    mut stream: UnixStream,
    requests_tx: futures_mpsc::UnboundedSender<RequestEnvelope>,
    responder: Responder,
    pending_requests: Arc<AtomicUsize>,
) {
    let mut decoder = FrameDecoder::new();
    let mut buf = [0; 8192];

    loop {
        let len = match stream.read(&mut buf) {
            Ok(0) => return,
            Ok(len) => len,
            Err(_) => return,
        };

        let frames = match decoder.push(&buf[..len]) {
            Ok(frames) => frames,
            Err(_) => return,
        };

        for frame in frames {
            let request = match serde_json::from_slice::<Request>(&frame.body) {
                Ok(request) => request,
                Err(_) => return,
            };

            if pending_requests.fetch_add(1, Ordering::AcqRel) >= MAX_INBOUND_QUEUED {
                pending_requests.fetch_sub(1, Ordering::AcqRel);
                return;
            }

            let envelope = RequestEnvelope {
                request,
                responder: responder.clone(),
                _permit: InboundPermit(pending_requests.clone()),
            };
            if requests_tx.unbounded_send(envelope).is_err() {
                return;
            }
        }
    }
}

fn write_loop(mut stream: UnixStream, outbound_rx: mpsc::Receiver<Response>) {
    for response in outbound_rx {
        let Ok(body) = serde_json::to_vec(&response) else {
            return;
        };
        let frame = encode_frame(&body);
        if stream.write_all(&frame).is_err() {
            return;
        }
    }
}
