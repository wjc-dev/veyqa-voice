//! Adapter from the hotkey gesture to the engine actor.

use std::sync::Arc;

use crossbeam_channel::Sender;

use super::clock;
use super::hub::StateHub;
use super::EngineCommand;
use crate::hotkey::listener::RecordingSink;

#[derive(Clone)]
pub struct TransitionSink {
    hub: Arc<StateHub>,
    commands: Sender<EngineCommand>,
}

impl TransitionSink {
    pub fn new(hub: Arc<StateHub>, commands: Sender<EngineCommand>) -> Self {
        Self { hub, commands }
    }
}

impl RecordingSink for TransitionSink {
    fn start(&mut self) -> bool {
        let now = clock();
        let Some(generation) = self.hub.try_begin_recording(now) else {
            return false;
        };
        if self
            .commands
            .send(EngineCommand::Start {
                generation,
                at: now,
            })
            .is_err()
        {
            self.hub.reset_keyboard(now);
            return false;
        }
        true
    }

    fn stop(&mut self) {
        if self.hub.end_recording() {
            let _ = self.commands.send(EngineCommand::Stop { at: clock() });
        }
    }

    fn cancel(&mut self) {
        if self.hub.begin_cancel().is_some() {
            let _ = self.commands.send(EngineCommand::Cancel);
            self.hub.finish_cancel();
            self.hub.reset_keyboard(clock());
        }
    }

    fn toggle(&mut self) {
        if self.hub.is_recording() {
            self.stop();
        } else {
            self.start();
        }
    }

    fn is_recording(&self) -> bool {
        self.hub.is_recording()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossbeam_channel::bounded;

    #[test]
    fn start_stop_cancel_and_toggle_mirror_the_keyboard_manager() {
        let hub = Arc::new(StateHub::new(0.0));
        let (sender, receiver) = bounded(8);
        let mut sink = TransitionSink::new(hub.clone(), sender);

        assert!(sink.start());
        assert!(sink.is_recording());
        assert!(matches!(
            receiver.recv().unwrap(),
            EngineCommand::Start { .. }
        ));
        assert!(!sink.start());

        sink.stop();
        assert!(!sink.is_recording());
        assert!(matches!(
            receiver.recv().unwrap(),
            EngineCommand::Stop { .. }
        ));
        sink.stop();
        assert!(receiver.try_recv().is_err());

        // The engine moves the session to processing and finally back to idle.
        hub.reset_keyboard(0.0);
        assert!(sink.start());
        let _ = receiver.recv().unwrap();
        sink.cancel();
        assert!(matches!(receiver.recv().unwrap(), EngineCommand::Cancel));
        assert!(!sink.is_recording());

        assert!(sink.start());
        let _ = receiver.recv().unwrap();
        sink.toggle();
        assert!(!sink.is_recording());
        assert!(matches!(
            receiver.recv().unwrap(),
            EngineCommand::Stop { .. }
        ));
        hub.reset_keyboard(0.0);
        sink.toggle();
        assert!(sink.is_recording());
    }

    #[test]
    fn a_closed_engine_channel_releases_the_latch() {
        let hub = Arc::new(StateHub::new(0.0));
        let (sender, receiver) = bounded(1);
        drop(receiver);
        let mut sink = TransitionSink::new(hub, sender);
        assert!(!sink.start());
        assert!(!sink.is_recording());
    }
}
