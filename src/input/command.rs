#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    MoveDown { count: u32 },
    MoveUp { count: u32 },
    MoveLeft { count: u32 },
    MoveRight { count: u32 },
    Open,
    Refresh,
    EnterInsert,
    EnterVisual,
    EnterCommand,
    EnterSearch { backward: bool },
    Back,
    Quit,
    Text(String),
    SubmitLine(String),
    Noop,
}
