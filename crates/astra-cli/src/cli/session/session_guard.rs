//! Graceful session shutdown signal routing.

use std::sync::{Once, OnceLock};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ShutdownSignal {
    Sigint,
    Sigterm,
    Sighup,
}

impl ShutdownSignal {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Sigint => "SIGINT",
            Self::Sigterm => "SIGTERM",
            Self::Sighup => "SIGHUP",
        }
    }
}

static SHUTDOWN_SIGNAL_TX: OnceLock<tokio::sync::watch::Sender<Option<ShutdownSignal>>> =
    OnceLock::new();
static SHUTDOWN_SIGNAL_HANDLER_INSTALLED: Once = Once::new();

fn shutdown_signal_sender() -> &'static tokio::sync::watch::Sender<Option<ShutdownSignal>> {
    SHUTDOWN_SIGNAL_TX.get_or_init(|| {
        let (tx, _rx) = tokio::sync::watch::channel(None);
        tx
    })
}

fn publish_shutdown_signal(signal: ShutdownSignal) {
    let _ = shutdown_signal_sender().send(Some(signal));
}

pub(crate) fn subscribe_shutdown_signal() -> tokio::sync::watch::Receiver<Option<ShutdownSignal>> {
    shutdown_signal_sender().subscribe()
}

/// Install signal handlers that request a graceful REPL shutdown.
/// Must be called inside a tokio runtime.
pub(crate) fn install_sigterm_handler() {
    SHUTDOWN_SIGNAL_HANDLER_INSTALLED.call_once(|| {
        tokio::spawn(async {
            #[cfg(unix)]
            {
                use tokio::signal::unix::{SignalKind, signal};
                if let Ok(mut sigterm) = signal(SignalKind::terminate()) {
                    sigterm.recv().await;
                    publish_shutdown_signal(ShutdownSignal::Sigterm);
                }
            }
        });
        #[cfg(unix)]
        tokio::spawn(async {
            use tokio::signal::unix::{SignalKind, signal};
            if let Ok(mut sighup) = signal(SignalKind::hangup()) {
                sighup.recv().await;
                publish_shutdown_signal(ShutdownSignal::Sighup);
            }
        });
    });
}

#[cfg(test)]
mod tests {
    use super::{
        ShutdownSignal, publish_shutdown_signal, shutdown_signal_sender, subscribe_shutdown_signal,
    };

    #[tokio::test]
    async fn publish_shutdown_signal_updates_subscribers() {
        let _ = shutdown_signal_sender().send(None);
        let mut rx = subscribe_shutdown_signal();

        publish_shutdown_signal(ShutdownSignal::Sigterm);

        rx.changed().await.unwrap();
        assert_eq!(*rx.borrow(), Some(ShutdownSignal::Sigterm));

        let _ = shutdown_signal_sender().send(None);
    }

    #[test]
    fn shutdown_signal_labels_are_stable() {
        assert_eq!(ShutdownSignal::Sigterm.label(), "SIGTERM");
        assert_eq!(ShutdownSignal::Sighup.label(), "SIGHUP");
    }
}
