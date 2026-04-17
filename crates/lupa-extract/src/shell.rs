use anyhow::{anyhow, Result};
use std::io::Read;
use std::process::{Command, Stdio};
use std::time::Duration;
use wait_timeout::ChildExt;

pub trait CommandRunner {
    fn run(&self, cmd: &str, args: &[&str], input: Option<&[u8]>) -> Result<String>;
}

pub struct SystemRunner {
    pub timeout: Duration,
}

impl SystemRunner {
    pub fn new(timeout_secs: u64) -> Self {
        Self {
            timeout: Duration::from_secs(timeout_secs),
        }
    }
}

impl Default for SystemRunner {
    fn default() -> Self {
        Self::new(15)
    }
}

impl CommandRunner for SystemRunner {
    fn run(&self, cmd: &str, args: &[&str], _input: Option<&[u8]>) -> Result<String> {
        let mut child = Command::new(cmd)
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        if self.timeout.is_zero() {
            let output = child.wait_with_output()?;
            return if output.status.success() {
                Ok(String::from_utf8_lossy(&output.stdout).to_string())
            } else {
                Err(anyhow!(
                    "{} failed: {}",
                    cmd,
                    String::from_utf8_lossy(&output.stderr)
                ))
            };
        }

        match child.wait_timeout(self.timeout)? {
            Some(status) => {
                let mut stdout = String::new();
                let mut stderr = String::new();
                if let Some(mut out) = child.stdout.take() {
                    let mut buf = Vec::new();
                    out.read_to_end(&mut buf).ok();
                    stdout = String::from_utf8_lossy(&buf).to_string();
                }
                if let Some(mut err) = child.stderr.take() {
                    let mut buf = Vec::new();
                    err.read_to_end(&mut buf).ok();
                    stderr = String::from_utf8_lossy(&buf).to_string();
                }
                if status.success() {
                    Ok(stdout)
                } else {
                    Err(anyhow!("{} failed: {}", cmd, stderr))
                }
            }
            None => {
                let _ = child.kill();
                let _ = child.wait();
                Err(anyhow!(
                    "extractor '{}' timed out after {}s",
                    cmd,
                    self.timeout.as_secs()
                ))
            }
        }
    }
}

#[doc(hidden)]
pub fn command_exists(cmd: &str) -> bool {
    which::which(cmd).is_ok()
}

pub fn extract_doc(bytes: &[u8], runner: &dyn CommandRunner) -> Result<String> {
    let tmp = write_temp(bytes, "doc")?;
    let text = runner.run("antiword", &["-m", "UTF-8", tmp.to_str().unwrap()], None)?;
    let _ = std::fs::remove_file(&tmp);
    Ok(text)
}

pub fn extract_xls(bytes: &[u8], runner: &dyn CommandRunner) -> Result<String> {
    let tmp = write_temp(bytes, "xls")?;
    let text = runner.run("catdoc", &[tmp.to_str().unwrap()], None)?;
    let _ = std::fs::remove_file(&tmp);
    Ok(text)
}

pub fn extract_ppt(bytes: &[u8], runner: &dyn CommandRunner) -> Result<String> {
    let tmp = write_temp(bytes, "ppt")?;
    let text = runner.run(
        "libreoffice",
        &["--headless", "--cat", tmp.to_str().unwrap()],
        None,
    )?;
    let _ = std::fs::remove_file(&tmp);
    Ok(text)
}

pub fn extract_pdf(bytes: &[u8], runner: &dyn CommandRunner) -> Result<String> {
    let tmp = write_temp(bytes, "pdf")?;
    let text = runner.run(
        "pdftotext",
        &["-layout", "-enc", "UTF-8", tmp.to_str().unwrap(), "-"],
        None,
    )?;
    let _ = std::fs::remove_file(&tmp);
    Ok(text)
}

fn write_temp(bytes: &[u8], ext: &str) -> Result<std::path::PathBuf> {
    let tmp = std::env::temp_dir().join(format!("lupa-shell-{}.{}", std::process::id(), ext));
    std::fs::write(&tmp, bytes)?;
    Ok(tmp)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn timeout_kills_long_running() {
        let runner = SystemRunner::new(1);
        let start = Instant::now();
        let res = runner.run("sh", &["-c", "sleep 30"], None);
        assert!(res.is_err());
        assert!(start.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn fast_command_succeeds() {
        let runner = SystemRunner::new(5);
        let out = runner.run("sh", &["-c", "echo hello"], None).unwrap();
        assert!(out.contains("hello"));
    }

    #[test]
    fn zero_timeout_means_no_timeout() {
        let runner = SystemRunner::new(0);
        let out = runner.run("sh", &["-c", "echo ok"], None).unwrap();
        assert!(out.contains("ok"));
    }
}
