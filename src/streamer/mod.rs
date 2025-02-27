mod alis;
mod server;
mod session;
use crate::config::Key;
use crate::notifier::Notifier;
use crate::pty;
use crate::tty;
use crate::util;
use std::io;
use std::net::{self, TcpListener};
use std::thread;
use std::time::Instant;
use tokio::sync::{mpsc, oneshot};
use tracing::info;

pub struct Streamer {
    record_input: bool,
    keys: KeyBindings,
    notifier: Option<Box<dyn Notifier>>,
    notifier_tx: std::sync::mpsc::Sender<String>,
    notifier_rx: Option<std::sync::mpsc::Receiver<String>>,
    notifier_handle: Option<util::JoinHandle>,
    pty_tx: mpsc::UnboundedSender<Event>,
    pty_rx: Option<mpsc::UnboundedReceiver<Event>>,
    event_loop_handle: Option<util::JoinHandle>,
    start_time: Instant,
    paused: bool,
    prefix_mode: bool,
    listen_addr: net::SocketAddr,
}

enum Event {
    Output(u64, String),
    Input(u64, String),
    Resize(u64, tty::TtySize),
}

impl Streamer {
    pub fn new(
        listen_addr: net::SocketAddr,
        record_input: bool,
        keys: KeyBindings,
        notifier: Box<dyn Notifier>,
    ) -> Self {
        let (notifier_tx, notifier_rx) = std::sync::mpsc::channel();
        let (pty_tx, pty_rx) = mpsc::unbounded_channel();

        Self {
            record_input,
            keys,
            notifier: Some(notifier),
            notifier_tx,
            notifier_rx: Some(notifier_rx),
            notifier_handle: None,
            pty_tx,
            pty_rx: Some(pty_rx),
            event_loop_handle: None,
            start_time: Instant::now(),
            paused: false,
            prefix_mode: false,
            listen_addr,
        }
    }

    fn elapsed_time(&self) -> u64 {
        self.start_time.elapsed().as_micros() as u64
    }

    fn notify<S: ToString>(&self, message: S) {
        let message = message.to_string();
        info!(message);

        self.notifier_tx
            .send(message)
            .expect("notification send should succeed");
    }
}

impl pty::Recorder for Streamer {
    fn start(&mut self, tty_size: tty::TtySize) -> io::Result<()> {
        let pty_rx = self.pty_rx.take().unwrap();
        let (clients_tx, mut clients_rx) = mpsc::channel(1);
        let (server_shutdown_tx, server_shutdown_rx) = oneshot::channel::<()>();
        let listener = TcpListener::bind(self.listen_addr)?;
        let runtime = build_tokio_runtime();
        let server = runtime.spawn(server::serve(listener, clients_tx, server_shutdown_rx));

        self.event_loop_handle = wrap_thread_handle(thread::spawn(move || {
            runtime.block_on(async move {
                event_loop(pty_rx, &mut clients_rx, tty_size).await;
                let _ = server_shutdown_tx.send(());
                let _ = server.await;
                let _ = clients_rx.recv().await;
            });
        }));

        let mut notifier = self.notifier.take().unwrap();
        let notifier_rx = self.notifier_rx.take().unwrap();

        self.notifier_handle = wrap_thread_handle(thread::spawn(move || {
            for message in notifier_rx {
                let _ = notifier.notify(message);
            }
        }));

        self.start_time = Instant::now();

        Ok(())
    }

    fn output(&mut self, data: &[u8]) {
        if self.paused {
            return;
        }

        let data = String::from_utf8_lossy(data).to_string();
        let event = Event::Output(self.elapsed_time(), data);
        self.pty_tx.send(event).expect("output send should succeed");
    }

    fn input(&mut self, data: &[u8]) -> bool {
        let prefix_key = self.keys.prefix.as_ref();
        let pause_key = self.keys.pause.as_ref();

        if !self.prefix_mode && prefix_key.is_some_and(|key| data == key) {
            self.prefix_mode = true;
            return false;
        }

        if self.prefix_mode || prefix_key.is_none() {
            self.prefix_mode = false;

            if pause_key.is_some_and(|key| data == key) {
                if self.paused {
                    self.paused = false;
                    self.notify("Resumed streaming");
                } else {
                    self.paused = true;
                    self.notify("Paused streaming");
                }

                return false;
            }
        }

        if self.record_input && !self.paused {
            let data = String::from_utf8_lossy(data).to_string();
            let event = Event::Input(self.elapsed_time(), data);
            self.pty_tx.send(event).expect("input send should succeed");
        }

        true
    }

    fn resize(&mut self, size: crate::tty::TtySize) {
        let event = Event::Resize(self.elapsed_time(), size);
        self.pty_tx.send(event).expect("resize send should succeed");
    }
}

async fn event_loop(
    mut events: mpsc::UnboundedReceiver<Event>,
    clients: &mut mpsc::Receiver<session::Client>,
    tty_size: tty::TtySize,
) {
    let mut session = session::Session::new(tty_size);

    loop {
        tokio::select! {
            event = events.recv() => {
                match event {
                    Some(Event::Output(time, data)) => {
                        session.output(time, data);
                    }

                    Some(Event::Input(time, data)) => {
                        session.input(time, data);
                    }

                    Some(Event::Resize(time, new_tty_size)) => {
                        session.resize(time, new_tty_size);
                    }

                    None => break,
                }
            }

            client = clients.recv() => {
                match client {
                    Some(client) => {
                        client.accept(session.subscribe());
                        info!("viewer count: {}", session.subscriber_count());
                    }

                    None => break,
                }
            }
        }
    }
}

fn build_tokio_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

fn wrap_thread_handle(handle: thread::JoinHandle<()>) -> Option<util::JoinHandle> {
    Some(util::JoinHandle::new(handle))
}

pub struct KeyBindings {
    pub prefix: Key,
    pub pause: Key,
}

impl Default for KeyBindings {
    fn default() -> Self {
        Self {
            prefix: None,
            pause: Some(vec![0x1c]), // ^\
        }
    }
}
