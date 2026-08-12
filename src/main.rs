use lemmy::AppError;

fn main() -> std::result::Result<(), AppError> {
    Err(AppError::Terminal(
        "interactive shell is not implemented".to_owned(),
    ))
}
