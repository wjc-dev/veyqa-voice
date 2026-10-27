//! Cross-thread UI commands.
//!
//! AppKit objects are `!Send` and must only be touched on the main thread.
//! Every other thread therefore emits a `UiCommand` into a channel that the
//! main run loop drains. The rendering side lands with the menu bar and
//! floating preview; this module owns the contract between them.

use crossbeam_channel::{Receiver, Sender};

use crate::state::InputState;

pub mod app;
pub mod menu_bar;
pub mod preview;

/// When a notice may still be shown. The preview evaluates this on the main
/// thread, so a notice queued just before a new recording can never cover it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NoticeScope {
    /// Python's `lambda: self._current_session is None` (start rejection).
    Idle,
    /// Python's `_finish_session` predicate: the notice belongs to this exact
    /// session and nothing newer may be running.
    FinishedSession(String),
}

#[derive(Clone, Debug, PartialEq)]
pub enum UiCommand {
    /// Menu-bar title/tooltip source of truth.
    State(InputState),
    PreviewShow { preparing: bool },
    PreviewHide,
    PreviewText(String),
    /// Microphone stopped; freeze the waveform but keep the caption.
    PreviewProcessing,
    /// Normalized 0..1 input level for the 30 Hz animation.
    PreviewLevel(f64),
    Notice { text: String, scope: NoticeScope },
}

pub trait UiSink: Send + Sync {
    fn send(&self, command: UiCommand);
}

/// Non-blocking bridge used by the engine actor.
pub struct ChannelUiSink {
    sender: Sender<UiCommand>,
}

impl ChannelUiSink {
    pub fn new(sender: Sender<UiCommand>) -> Self {
        Self { sender }
    }
}

impl UiSink for ChannelUiSink {
    fn send(&self, command: UiCommand) {
        // The main loop is allowed to fall behind; stale UI must never block
        // the dictation path.
        let _ = self.sender.try_send(command);
    }
}

/// Headless runs have no menu bar or floating preview to update.
pub struct NullUiSink;

impl UiSink for NullUiSink {
    fn send(&self, _command: UiCommand) {}
}

pub fn channel() -> (Sender<UiCommand>, Receiver<UiCommand>) {
    crossbeam_channel::unbounded()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_sink_forwards_commands_without_blocking() {
        let (sender, receiver) = channel();
        let sink = ChannelUiSink::new(sender);
        sink.send(UiCommand::PreviewText("你好".into()));
        sink.send(UiCommand::State(InputState::Streaming));
        assert_eq!(receiver.recv().unwrap(), UiCommand::PreviewText("你好".into()));
        assert_eq!(
            receiver.recv().unwrap(),
            UiCommand::State(InputState::Streaming)
        );
    }
}
