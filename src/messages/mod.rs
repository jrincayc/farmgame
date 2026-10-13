//! Game messages: a persistent log of things the player should know about
//! (crops ready or dead, a plant waiting for funds, queue problems),
//! plus the options that react to them (auto-pause).
//!
//! Emitters send a `GameMessage` event and know nothing about the UI.
//! Emitters are responsible for batching (one message per field and crop,
//! not per tile). A message with a `key` is a *standing* message: sending
//! the same key again updates the existing entry in place instead of
//! appending, and `ClearMessageKey` removes it once the condition ends.

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use crate::field::FieldId;
use crate::sim_order::DayAdvanceOrder;
use crate::time::{TimeControl, TimeMode};

const MAX_LOGGED: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MessageKind {
    ReadyToHarvest,
    CropDied,
    WaitingForFunds,
    ActionDone,
    ActionDropped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Severity {
    Info,
    Warning,
    Alert,
}

#[derive(Event, Debug, Clone)]
pub struct GameMessage {
    pub day: u32,
    pub kind: MessageKind,
    pub severity: Severity,
    pub text: String,
    /// A tile to jump to when the message is clicked.
    pub tile: Option<IVec2>,
    pub field: Option<FieldId>,
    pub key: Option<String>,
}

/// Removes the standing message with this key, if any.
#[derive(Event, Debug, Clone)]
pub struct ClearMessageKey(pub String);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoggedMessage {
    pub id: u64,
    pub day: u32,
    pub kind: MessageKind,
    pub severity: Severity,
    pub text: String,
    pub tile: Option<IVec2>,
    pub field: Option<FieldId>,
    pub key: Option<String>,
    pub unread: bool,
}

/// Oldest first.
#[derive(Resource, Debug, Clone, Default, Serialize, Deserialize)]
pub struct MessageLog {
    pub entries: Vec<LoggedMessage>,
    next_id: u64,
}

impl MessageLog {
    pub fn unread(&self) -> usize {
        self.entries.iter().filter(|m| m.unread).count()
    }

    pub fn mark_all_read(&mut self) {
        for m in &mut self.entries {
            m.unread = false;
        }
    }

    /// Appends, or refreshes the standing entry if `msg.key` matches one:
    /// the entry keeps its id but moves to the newest position, so a
    /// refreshed message isn't buried among older ones.
    fn record(&mut self, msg: GameMessage) {
        if let Some(key) = &msg.key {
            if let Some(pos) = self.entries.iter().position(|m| m.key.as_ref() == Some(key)) {
                let mut existing = self.entries.remove(pos);
                if existing.text != msg.text {
                    existing.unread = true;
                }
                existing.day = msg.day;
                existing.text = msg.text;
                existing.tile = msg.tile;
                existing.field = msg.field;
                self.entries.push(existing);
                return;
            }
        }
        let id = self.next_id;
        self.next_id += 1;
        self.entries.push(LoggedMessage {
            id,
            day: msg.day,
            kind: msg.kind,
            severity: msg.severity,
            text: msg.text,
            tile: msg.tile,
            field: msg.field,
            key: msg.key,
            unread: true,
        });
        let excess = self.entries.len().saturating_sub(MAX_LOGGED);
        self.entries.drain(..excess);
    }
}

/// Gameplay options. Saved with the game, like `UiTheme`.
#[derive(Resource, Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct GameOptions {
    #[serde(default)]
    pub auto_pause_on_ready: bool,
    #[serde(default)]
    pub auto_pause_on_death: bool,
}

pub struct MessagesPlugin;

impl Plugin for MessagesPlugin {
    fn build(&self, app: &mut App) {
        app.add_event::<GameMessage>()
            .add_event::<ClearMessageKey>()
            .init_resource::<MessageLog>()
            .init_resource::<GameOptions>()
            .add_systems(Update, collect_messages.after(DayAdvanceOrder::CropsGrowth));
    }
}

fn collect_messages(
    mut events: EventReader<GameMessage>,
    mut clears: EventReader<ClearMessageKey>,
    mut log: ResMut<MessageLog>,
    options: Res<GameOptions>,
    mut control: ResMut<TimeControl>,
) {
    for msg in events.read() {
        let pause = match msg.kind {
            MessageKind::ReadyToHarvest => options.auto_pause_on_ready,
            MessageKind::CropDied => options.auto_pause_on_death,
            _ => false,
        };
        if pause {
            control.mode = TimeMode::Paused;
        }
        log.record(msg.clone());
    }
    for ClearMessageKey(key) in clears.read() {
        log.entries.retain(|m| m.key.as_ref() != Some(key));
    }
}

/// "Field A: 12 Tomato died (frost)" / "3 Carrot ready to harvest".
pub fn batch_text(field_name: Option<&str>, count: usize, crop: &str, what: &str) -> String {
    match field_name {
        Some(f) => format!("{f}: {count} {crop} {what}"),
        None => format!("{count} {crop} {what}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(key: Option<&str>, text: &str) -> GameMessage {
        GameMessage {
            day: 1,
            kind: MessageKind::WaitingForFunds,
            severity: Severity::Warning,
            text: text.into(),
            tile: None,
            field: None,
            key: key.map(String::from),
        }
    }

    #[test]
    fn standing_messages_update_in_place_and_only_reflag_on_change() {
        let mut log = MessageLog::default();
        log.record(msg(Some("k"), "waiting"));
        log.mark_all_read();
        log.record(msg(Some("k"), "waiting"));
        assert_eq!((log.entries.len(), log.unread()), (1, 0));
        log.record(msg(Some("k"), "still waiting, less money"));
        assert_eq!((log.entries.len(), log.unread()), (1, 1));
        log.record(msg(None, "other"));
        assert_eq!(log.entries.len(), 2);
        // Refreshing the standing message moves it to the newest position.
        log.record(msg(Some("k"), "still waiting, even less money"));
        assert_eq!(log.entries.last().unwrap().key.as_deref(), Some("k"));
    }

    #[test]
    fn log_is_capped() {
        let mut log = MessageLog::default();
        for i in 0..(MAX_LOGGED + 20) {
            log.record(msg(None, &format!("m{i}")));
        }
        assert_eq!(log.entries.len(), MAX_LOGGED);
        assert_eq!(log.entries[0].text, "m20");
    }
}
