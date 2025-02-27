use crate::config::Config;
use crate::locale;
use crate::logger;
use crate::pty;
use crate::streamer::{self, KeyBindings};
use crate::tty;
use anyhow::{anyhow, Result};
use clap::Args;
use std::fs;
use std::net::SocketAddr;
use std::path::PathBuf;
use tracing::level_filters::LevelFilter;
use tracing_subscriber::EnvFilter;

#[derive(Debug, Args)]
pub struct Cli {
    /// Enable input capture
    #[arg(long, short = 'I', alias = "stdin")]
    input: bool,

    /// Command to stream [default: $SHELL]
    #[arg(short, long)]
    command: Option<String>,

    /// HTTP server listen address
    #[clap(short, long, default_value = "127.0.0.1:8080")]
    listen_addr: SocketAddr,

    /// Override terminal size for the session
    #[arg(long, value_name = "COLSxROWS")]
    tty_size: Option<pty::WinsizeOverride>,

    /// Log file path
    #[arg(long)]
    log_file: Option<PathBuf>,
}

impl Cli {
    pub fn run(self, config: &Config) -> Result<()> {
        locale::check_utf8_locale()?;

        let command = self.get_command(config);
        let keys = get_key_bindings(config)?;
        let notifier = super::get_notifier(config);
        let record_input = self.input || config.cmd_stream_input();
        let exec_command = super::build_exec_command(command.as_ref().cloned());
        let exec_extra_env = super::build_exec_extra_env();
        let mut streamer = streamer::Streamer::new(self.listen_addr, record_input, keys, notifier);

        logger::info!(
            "Streaming session started, web server listening on http://{}",
            &self.listen_addr
        );

        if command.is_none() {
            logger::info!("Press <ctrl+d> or type 'exit' to end");
        }

        {
            let mut tty: Box<dyn tty::Tty> = if let Ok(dev_tty) = tty::DevTty::open() {
                Box::new(dev_tty)
            } else {
                logger::info!("TTY not available, streaming in headless mode");
                Box::new(tty::NullTty::open()?)
            };

            self.init_logging(config)?;

            pty::exec(
                &exec_command,
                &exec_extra_env,
                &mut *tty,
                self.tty_size,
                &mut streamer,
            )?;
        }

        logger::info!("Streaming session ended");

        Ok(())
    }

    fn get_command(&self, config: &Config) -> Option<String> {
        self.command
            .as_ref()
            .cloned()
            .or(config.cmd_stream_command())
    }

    fn init_logging(&self, config: &Config) -> Result<()> {
        let log_file = self
            .log_file
            .as_ref()
            .cloned()
            .or(config.cmd_stream_log_file());

        if let Some(path) = &log_file {
            let file = fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .map_err(|e| anyhow!("cannot open log file {}: {}", path.to_string_lossy(), e))?;

            let filter = EnvFilter::builder()
                .with_default_directive(LevelFilter::INFO.into())
                .from_env_lossy();

            tracing_subscriber::fmt()
                .with_ansi(false)
                .with_env_filter(filter)
                .with_writer(file)
                .init();
        }

        Ok(())
    }
}

fn get_key_bindings(config: &Config) -> Result<KeyBindings> {
    let mut keys = KeyBindings::default();

    if let Some(key) = config.cmd_stream_prefix_key()? {
        keys.prefix = key;
    }

    if let Some(key) = config.cmd_stream_pause_key()? {
        keys.pause = key;
    }

    Ok(keys)
}
