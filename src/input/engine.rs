use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::{command::Command, mapping::MappingTable, mode::Mode};

#[derive(Debug, Clone)]
pub struct InputEngine {
    mode: Mode,
    count: u32,
    line: String,
    mappings: MappingTable,
    pending: Vec<KeyCode>,
}

impl Default for InputEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl InputEngine {
    pub fn new() -> Self {
        let mut mappings = MappingTable::new();
        mappings.insert('h', Command::MoveLeft { count: 1 });
        mappings.insert('j', Command::MoveDown { count: 1 });
        mappings.insert('k', Command::MoveUp { count: 1 });
        mappings.insert('l', Command::MoveRight { count: 1 });
        mappings.insert("gg", Command::GoToFirst { count: 1 });
        mappings.insert('G', Command::GoToLast { count: 1 });
        mappings.insert('r', Command::Refresh);
        mappings.insert('>', Command::NextPage);
        mappings.insert('<', Command::PreviousPage);
        mappings.insert('q', Command::Quit);
        mappings.insert('y', Command::Confirm);
        mappings.insert('n', Command::Cancel);
        mappings.insert('i', Command::EnterInsert);
        mappings.insert('v', Command::EnterVisual);
        mappings.insert(':', Command::EnterCommand);
        mappings.insert('/', Command::EnterSearch { backward: false });
        mappings.insert('?', Command::EnterSearch { backward: true });
        mappings.insert(KeyCode::Esc, Command::Back);
        mappings.insert(KeyCode::Enter, Command::Open);

        Self {
            mode: Mode::Normal,
            count: 0,
            line: String::new(),
            mappings,
            pending: Vec::new(),
        }
    }

    /// Apply persisted `[keymaps]` entries (command name → key sequence) on
    /// top of the default bindings. The new sequence replaces the default
    /// binding for the named command (so e.g. rebinding `down` to `jk`
    /// unshadows the multi-key sequence), and multi-key sequences participate
    /// in prefix matching like any other mapping. Entries whose name is not a
    /// documented command are skipped with a warning.
    pub fn with_keymaps(mut self, keymaps: &std::collections::HashMap<String, String>) -> Self {
        for (name, sequence) in keymaps {
            match Command::by_name(name) {
                Some(command) => {
                    self.mappings.remove_command(&command);
                    self.mappings.insert(sequence.as_str(), command);
                }
                None => tracing::warn!(name = %name, "ignoring keymap for unknown command name"),
            }
        }
        self
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    pub fn handle(&mut self, key: KeyEvent) -> Command {
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            return match (self.mode, key.code) {
                // Vim-style half-page scrolling of the open detail pane.
                (Mode::Normal | Mode::Visual, KeyCode::Char('d')) => Command::ScrollDetailDown {
                    count: self.count_for_command(),
                },
                (Mode::Normal | Mode::Visual, KeyCode::Char('u')) => Command::ScrollDetailUp {
                    count: self.count_for_command(),
                },
                _ => Command::Noop,
            };
        }
        if key.modifiers.contains(KeyModifiers::ALT) {
            return Command::Noop;
        }

        match self.mode {
            Mode::Insert => self.handle_insert(key.code),
            Mode::Command | Mode::SearchForward | Mode::SearchBackward => {
                self.handle_line_mode(key.code)
            }
            Mode::Normal | Mode::Visual => self.handle_mapped(key.code),
        }
    }

    fn handle_insert(&mut self, key: KeyCode) -> Command {
        self.pending.clear();
        match key {
            KeyCode::Esc => self.leave_modal(Command::Back),
            KeyCode::Backspace => Command::Backspace,
            KeyCode::Char(character) => Command::Text(character.to_string()),
            _ => Command::Noop,
        }
    }

    fn handle_line_mode(&mut self, key: KeyCode) -> Command {
        self.pending.clear();
        match key {
            // Esc abandons the line: a distinct command so the app can clear
            // the visible compose text without treating it as navigation
            // back (which would also close an open thread).
            KeyCode::Esc => self.leave_modal(Command::CancelLine),
            KeyCode::Enter => {
                self.mode = Mode::Normal;
                self.count = 0;
                Command::SubmitLine(std::mem::take(&mut self.line))
            }
            KeyCode::Backspace => {
                self.line.pop();
                Command::Backspace
            }
            KeyCode::Char(character) => {
                self.line.push(character);
                Command::Text(character.to_string())
            }
            _ => Command::Noop,
        }
    }

    fn handle_mapped(&mut self, key: KeyCode) -> Command {
        if let KeyCode::Char(character @ '0'..='9') = key {
            // A digit counts a motion only when no registered mapping begins
            // with it; digit-leading keymap sequences (for example a `2j`
            // mapping) must stay reachable, so such a digit joins pending
            // prefix matching instead of accumulating a count.
            let begins_a_mapping =
                self.mappings.has_prefix(key) || self.mappings.resolve(key).is_some();
            if !begins_a_mapping {
                self.count = self
                    .count
                    .saturating_mul(10)
                    .saturating_add(character as u32 - '0' as u32);
                self.pending.clear();
                return Command::Noop;
            }
        }

        self.pending.push(key);
        let Some((_length, command)) = self.mappings.longest_complete(self.pending.as_slice())
        else {
            if !self.mappings.has_prefix(self.pending.as_slice()) {
                self.pending.clear();
                // A discarded pending sequence also abandons any accumulated
                // count, so a later key never inherits the stale multiplier.
                self.count = 0;
            }
            return Command::Noop;
        };

        self.pending.clear();
        let count = self.count_for_command();
        self.apply(command, count)
    }

    fn count_for_command(&mut self) -> u32 {
        let count = if self.count == 0 { 1 } else { self.count };
        self.count = 0;
        count
    }

    fn apply(&mut self, command: Command, count: u32) -> Command {
        match command {
            Command::MoveDown { .. } => Command::MoveDown { count },
            Command::MoveUp { .. } => Command::MoveUp { count },
            Command::MoveLeft { .. } => Command::MoveLeft { count },
            Command::MoveRight { .. } => Command::MoveRight { count },
            Command::GoToFirst { .. } => Command::GoToFirst { count },
            Command::GoToLast { .. } => Command::GoToLast { count },
            Command::EnterInsert => {
                self.mode = Mode::Insert;
                Command::EnterInsert
            }
            Command::EnterVisual => {
                self.mode = Mode::Visual;
                Command::EnterVisual
            }
            Command::EnterCommand => {
                self.mode = Mode::Command;
                self.line.clear();
                Command::EnterCommand
            }
            Command::EnterSearch { backward } => {
                self.mode = if backward {
                    Mode::SearchBackward
                } else {
                    Mode::SearchForward
                };
                self.line.clear();
                Command::EnterSearch { backward }
            }
            Command::Back => self.leave_modal(Command::Back),
            other => other,
        }
    }

    fn leave_modal(&mut self, command: Command) -> Command {
        self.mode = Mode::Normal;
        self.count = 0;
        self.line.clear();
        self.pending.clear();
        command
    }
}
