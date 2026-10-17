//! IME (Input Method Editor) state machine.
//!
//! Handles text composition for CJK and other complex scripts.
//! Preedit text is displayed as UI state; commit text is sent to the PTY
//! exactly once.

/// The IME state machine.
#[derive(Debug, Clone, Default)]
pub struct ImeState {
    /// The current preedit text (being composed).
    preedit: String,
    /// The committed text (ready to send to PTY).
    committed: String,
    /// Whether the IME is active (composing).
    active: bool,
}

impl ImeState {
    /// Create a new IME state.
    pub fn new() -> Self {
        Self::default()
    }

    /// Start a new composition.
    pub fn start_composition(&mut self) {
        self.active = true;
        self.preedit.clear();
        self.committed.clear();
    }

    /// Update the preedit text.
    pub fn update_preedit(&mut self, preedit: &str) {
        self.preedit = preedit.to_string();
    }

    /// Commit the current composition.
    /// Returns the committed text, or None if there is nothing to commit.
    pub fn commit(&mut self) -> Option<String> {
        if !self.active {
            return None;
        }

        // If there's preedit text, commit it
        if !self.preedit.is_empty() {
            self.committed = self.preedit.clone();
            self.preedit.clear();
            self.active = false;
            return Some(self.committed.clone());
        }

        // If there's already committed text, return it
        if !self.committed.is_empty() {
            let result = self.committed.clone();
            self.committed.clear();
            self.active = false;
            return Some(result);
        }

        None
    }

    /// Cancel the current composition.
    pub fn cancel(&mut self) {
        self.active = false;
        self.preedit.clear();
        self.committed.clear();
    }

    /// Drop the composition shown so far but stay enabled (the platform
    /// IME is still on for the window): used when focus moves to another
    /// pane, so no preedit is stranded on the pane left behind and the
    /// next platform events start fresh where focus is now.
    pub fn drop_composition(&mut self) {
        self.preedit.clear();
        self.committed.clear();
    }

    /// Check if the IME is active.
    pub fn is_active(&self) -> bool {
        self.active
    }

    /// Get the current preedit text.
    pub fn preedit(&self) -> &str {
        &self.preedit
    }

    /// Get the current committed text.
    pub fn committed(&self) -> &str {
        &self.committed
    }

    /// Clear all state.
    pub fn clear(&mut self) {
        self.active = false;
        self.preedit.clear();
        self.committed.clear();
    }
}
