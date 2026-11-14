//! Platform-independent core of Tandem.
//!
//! The big idea (see `docs/DESIGN.md`): every booth runs its own audio engine
//! and plays the whole mix locally. Only small, timestamped control events
//! cross the network. Every machine agrees on one shared clock, so applying
//! the same events at the same timestamps gives the same mix everywhere.
//!
//! * [`time`] / [`clock`] — the shared room clock and how peers agree on it.
//! * [`model`] — decks, mixer and the event [`model::Timeline`]; the room's
//!   state at any moment is a pure function of the events before it.
//! * [`render`] — turns a timeline into audio for one output ("lane"),
//!   including the delayed broadcast lane.
//! * [`meter`] — channel and master level meters, measured by the live lane.
//! * [`grid`] — beatgrids: markers, bars, and the edits a DJ can make.
//! * [`net`] / [`files`] — rooms over TCP and track transfer.
//! * [`session`] — glue the UI talks to.

pub mod analysis;
pub mod audio;
pub mod clock;
pub mod config;
pub mod files;
pub mod grid;
pub mod keys;
pub mod library;
pub mod logging;
pub mod mapping;
pub mod meter;
pub mod model;
pub mod net;
pub mod protocol;
pub mod render;
pub mod selftest;
pub mod serato;
pub mod session;
pub mod stretch;
pub mod terms;
pub mod time;

pub use model::{Action, DeckState, Event, RoomState, Timeline, TrackId, DECKS};
