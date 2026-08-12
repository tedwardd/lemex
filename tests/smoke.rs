#[test]
fn library_exposes_error_result_alias() {
    let result: lemmy::Result<()> = Ok(());
    assert!(result.is_ok());
}
