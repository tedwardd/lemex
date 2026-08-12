use lemmy::{AppError, Result};

fn main() -> Result<(), AppError> {
    Err(AppError::Terminal(
        "interactive shell is not implemented".to_owned(),
    ))
}
