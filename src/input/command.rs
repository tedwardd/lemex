#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    MoveDown {
        count: u32,
    },
    MoveUp {
        count: u32,
    },
    MoveLeft {
        count: u32,
    },
    MoveRight {
        count: u32,
    },
    Open,
    Refresh,
    /// Flip to the next feed page, replacing the list (default key: `>`).
    NextPage,
    /// Flip back to the previous feed page (default key: `<`).
    PreviousPage,
    EnterInsert,
    EnterVisual,
    EnterCommand,
    EnterSearch {
        backward: bool,
    },
    Back,
    Quit,
    /// Scroll the open detail/thread pane down (default key: Ctrl-d).
    ScrollDetailDown {
        count: u32,
    },
    /// Scroll the open detail/thread pane up (default key: Ctrl-u).
    ScrollDetailUp {
        count: u32,
    },
    /// Confirm the pending destructive action (default key: `y`).
    Confirm,
    /// Cancel the pending destructive action (default key: `n`).
    Cancel,
    Text(String),
    SubmitLine(String),
    Noop,
}

impl Command {
    /// Resolve a documented command name to the command it binds, for
    /// configurable key mappings (`[keymaps]` in the config file). Unknown
    /// names return `None` so persisted-but-unrecognized entries are skipped
    /// at startup instead of crashing the input engine.
    pub fn by_name(name: &str) -> Option<Command> {
        match name {
            "down" => Some(Command::MoveDown { count: 1 }),
            "up" => Some(Command::MoveUp { count: 1 }),
            "left" => Some(Command::MoveLeft { count: 1 }),
            "right" => Some(Command::MoveRight { count: 1 }),
            "open" => Some(Command::Open),
            "refresh" => Some(Command::Refresh),
            "next-page" | "load-more" => Some(Command::NextPage),
            "previous-page" => Some(Command::PreviousPage),
            "insert" => Some(Command::EnterInsert),
            "visual" => Some(Command::EnterVisual),
            "command" => Some(Command::EnterCommand),
            "search" | "search-forward" => Some(Command::EnterSearch { backward: false }),
            "search-backward" => Some(Command::EnterSearch { backward: true }),
            "back" => Some(Command::Back),
            "quit" => Some(Command::Quit),
            "scroll-detail-down" => Some(Command::ScrollDetailDown { count: 1 }),
            "scroll-detail-up" => Some(Command::ScrollDetailUp { count: 1 }),
            "confirm" => Some(Command::Confirm),
            "cancel" => Some(Command::Cancel),
            _ => None,
        }
    }
}
