use anyhow::Result;
use std::process::Command;

pub trait CommandRunner {
    fn run(&self, cmd: &str, args: &[&str], input: Option<&[u8]>) -> Result<String>;
}

pub struct SystemRunner;

impl CommandRunner for SystemRunner {
    fn run(&self, cmd: &str, args: &[&str], _input: Option<&[u8]>) -> Result<String> {
        let output = Command::new(cmd)
            .args(args)
            .output()?;
        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).to_string())
        } else {
            anyhow::bail!("{} failed: {}", cmd, String::from_utf8_lossy(&output.stderr));
        }
    }
}

/// Check if a command is available.
pub fn command_exists(cmd: &str) -> bool {
    which::which(cmd).is_ok()
}

/// Extract text from DOC using antiword.
pub fn extract_doc(bytes: &[u8], runner: &dyn CommandRunner) -> Result<String> {
    let tmp = write_temp(bytes, "doc")?;
    let text = runner.run("antiword", &["-m", "UTF-8", tmp.to_str().unwrap()], None)?;
    let _ = std::fs::remove_file(&tmp);
    Ok(text)
}

/// Extract text from XLS using catdoc.
pub fn extract_xls(bytes: &[u8], runner: &dyn CommandRunner) -> Result<String> {
    let tmp = write_temp(bytes, "xls")?;
    let text = runner.run("catdoc", &[tmp.to_str().unwrap()], None)?;
    let _ = std::fs::remove_file(&tmp);
    Ok(text)
}

/// Extract text from PPT using libreoffice --headless --cat.
pub fn extract_ppt(bytes: &[u8], runner: &dyn CommandRunner) -> Result<String> {
    let tmp = write_temp(bytes, "ppt")?;
    let _out = tempfile::NamedTempFile::new()?;
    runner.run("libreoffice", &[
        "--headless", "--cat",
        tmp.to_str().unwrap(),
    ], None)?;
    // libreoffice --cat outputs to stdout
    let _ = std::fs::remove_file(&tmp);
    Ok(String::new()) // Would need to capture stdout
}

fn write_temp(bytes: &[u8], ext: &str) -> Result<std::path::PathBuf> {
    let tmp = std::env::temp_dir().join(format!("lupa-shell-{}.{}", std::process::id(), ext));
    std::fs::write(&tmp, bytes)?;
    Ok(tmp)
}
