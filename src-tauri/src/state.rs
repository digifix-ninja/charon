//! What every command can reach.

use charon_core::Engine;

pub struct AppState {
    pub engine: Engine,
}

impl AppState {
    pub fn new(engine: Engine) -> Self {
        Self { engine }
    }
}
