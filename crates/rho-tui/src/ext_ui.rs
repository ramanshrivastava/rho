//! A UI adapter that lets the extension host drive TUI dialogs (tau's
//! `ExtensionUi` → Textual screens).
//!
//! The `rho-ext-host::HostBridge` trait exposes `notify` / `ui_select` /
//! `ui_confirm` / `ui_input`, which a running WASM extension calls through
//! `context.ui.*`. Those calls originate off the TUI event loop and must await a
//! frontend answer. This module is the frontend seam: [`ExtensionUiHandle`] is a
//! cloneable, `HostBridge`-shaped async facade the `rho-coding` extension runtime
//! holds; every call ships a [`UiRequest`] (carrying a one-shot responder) down a
//! channel to the TUI, which opens the matching modal and answers.
//!
//! # Integration note
//!
//! `rho-tui` does not depend on `rho-ext-host`, so this adapter is provider-shaped
//! but not wired to `HostBridge` here. At integration the parent (coding-runtime
//! cluster) implements `HostBridge` for a struct wrapping an [`ExtensionUiHandle`]
//! and forwards each method to the identically-named async method below; the TUI
//! side is wired via [`crate::app::App::set_extension_ui_channel`].

use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::sync::oneshot;

/// A UI request emitted by the extension host, carrying a one-shot responder the
/// TUI fulfils once the user answers (or cancels).
#[derive(Debug)]
pub enum UiRequest {
    /// Show a notification; resolves immediately (no responder).
    Notify {
        /// Message text.
        message: String,
        /// Severity level (`info`/`warning`/`error`).
        level: String,
    },
    /// Show a picker; the responder gets the chosen option or `None` on cancel.
    Select {
        /// Dialog title.
        title: String,
        /// Selectable options.
        options: Vec<String>,
        /// One-shot answer channel.
        responder: oneshot::Sender<Option<String>>,
    },
    /// Show a confirmation; the responder gets `true` only if confirmed.
    Confirm {
        /// Dialog title.
        title: String,
        /// Body message.
        message: String,
        /// One-shot answer channel.
        responder: oneshot::Sender<bool>,
    },
    /// Show a text prompt; the responder gets the entered text or `None`.
    Input {
        /// Dialog title.
        title: String,
        /// Placeholder hint.
        placeholder: String,
        /// One-shot answer channel.
        responder: oneshot::Sender<Option<String>>,
    },
}

/// A cloneable, `HostBridge`-shaped async facade the extension runtime calls.
#[derive(Debug, Clone)]
pub struct ExtensionUiHandle {
    sender: UnboundedSender<UiRequest>,
}

impl ExtensionUiHandle {
    /// Show a notification (mirrors `HostBridge::notify`).
    pub fn notify(&self, message: &str, level: &str) {
        let _ = self.sender.send(UiRequest::Notify {
            message: message.to_string(),
            level: level.to_string(),
        });
    }

    /// Show a picker; `None` on cancel or if the TUI is gone (mirrors
    /// `HostBridge::ui_select`).
    pub async fn select(&self, title: &str, options: &[String]) -> Option<String> {
        let (responder, rx) = oneshot::channel();
        if self
            .sender
            .send(UiRequest::Select {
                title: title.to_string(),
                options: options.to_vec(),
                responder,
            })
            .is_err()
        {
            return None;
        }
        rx.await.ok().flatten()
    }

    /// Show a confirmation; `false` on cancel or if the TUI is gone (mirrors
    /// `HostBridge::ui_confirm`).
    pub async fn confirm(&self, title: &str, message: &str) -> bool {
        let (responder, rx) = oneshot::channel();
        if self
            .sender
            .send(UiRequest::Confirm {
                title: title.to_string(),
                message: message.to_string(),
                responder,
            })
            .is_err()
        {
            return false;
        }
        rx.await.unwrap_or(false)
    }

    /// Show a text prompt; `None` on cancel or if the TUI is gone (mirrors
    /// `HostBridge::ui_input`).
    pub async fn input(&self, title: &str, placeholder: &str) -> Option<String> {
        let (responder, rx) = oneshot::channel();
        if self
            .sender
            .send(UiRequest::Input {
                title: title.to_string(),
                placeholder: placeholder.to_string(),
                responder,
            })
            .is_err()
        {
            return None;
        }
        rx.await.ok().flatten()
    }
}

/// The TUI-side receiver of [`UiRequest`]s, owned by the [`crate::app::App`].
#[derive(Debug)]
pub struct ExtensionUiChannel {
    receiver: UnboundedReceiver<UiRequest>,
}

impl ExtensionUiChannel {
    /// Receive the next UI request, or `None` once every handle is dropped.
    pub async fn recv(&mut self) -> Option<UiRequest> {
        self.receiver.recv().await
    }
}

/// Create a linked ([`ExtensionUiHandle`], [`ExtensionUiChannel`]) pair. Give the
/// handle to the extension runtime and the channel to the TUI.
#[must_use]
pub fn extension_ui_pair() -> (ExtensionUiHandle, ExtensionUiChannel) {
    let (sender, receiver) = unbounded_channel();
    (
        ExtensionUiHandle { sender },
        ExtensionUiChannel { receiver },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn select_round_trips_through_channel() {
        let (handle, mut channel) = extension_ui_pair();
        let task =
            tokio::spawn(async move { handle.select("Pick", &["a".into(), "b".into()]).await });
        let request = channel.recv().await.expect("request");
        match request {
            UiRequest::Select {
                title,
                options,
                responder,
            } => {
                assert_eq!(title, "Pick");
                assert_eq!(options, vec!["a".to_string(), "b".to_string()]);
                responder.send(Some("b".to_string())).expect("respond");
            }
            other => panic!("expected select, got {other:?}"),
        }
        assert_eq!(task.await.expect("join"), Some("b".to_string()));
    }

    #[tokio::test]
    async fn confirm_defaults_false_when_channel_closed() {
        let (handle, channel) = extension_ui_pair();
        drop(channel);
        assert!(!handle.confirm("Title", "Sure?").await);
    }

    #[tokio::test]
    async fn notify_is_fire_and_forget() {
        let (handle, mut channel) = extension_ui_pair();
        handle.notify("hello", "info");
        match channel.recv().await.expect("request") {
            UiRequest::Notify { message, level } => {
                assert_eq!(message, "hello");
                assert_eq!(level, "info");
            }
            other => panic!("expected notify, got {other:?}"),
        }
    }
}
