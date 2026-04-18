use std::process::Command;

#[test]
fn test_lupa_help() {
    let output = Command::new("cargo")
        .args(["run", "-p", "lupa-cli", "--", "--help"])
        .output()
        .expect("failed to run lupa");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("toggle"));
    assert!(stdout.contains("search"));
}

#[test]
fn test_search_on_fixture() {
    // Start lupad with temp index
    // Index fixture dir
    // Search for known content
    // Assert results
}
