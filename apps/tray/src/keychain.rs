//! The session token, in the login Keychain, one item per server.
//!
//! Through `/usr/bin/security`, as slopify does: the app is ad-hoc signed, so only items that
//! trust `security` survive a rebuild without an allow prompt.

use std::io::Write;
use std::process::{Command, Stdio};

const SERVICE: &str = "mergepatrol-tray-session-token";
const SECURITY: &str = "/usr/bin/security";

pub fn read(server: &str) -> Option<String> {
    let out = Command::new(SECURITY)
        .args(["find-generic-password", "-s", SERVICE, "-a", server, "-w"])
        .output()
        .ok()?;
    let token = String::from_utf8(out.stdout).ok()?.trim().to_owned();
    (out.status.success() && !token.is_empty()).then_some(token)
}

/// `-w` with no value reads a tty, never a pipe, so the command goes in on stdin with `-i`,
/// which keeps the token out of argv.
pub fn write(server: &str, token: &str) -> Result<(), String> {
    let command = format!(
        "add-generic-password -a {} -s {SERVICE} -U -T {SECURITY} -w {}\n",
        quote(server),
        quote(token)
    );
    let mut child = Command::new(SECURITY)
        .arg("-i")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(command.as_bytes())
            .map_err(|e| e.to_string())?;
    }
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_owned())
    }
}

pub fn delete(server: &str) {
    let _ = Command::new(SECURITY)
        .args(["delete-generic-password", "-s", SERVICE, "-a", server])
        .output();
}

fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_for_security_interactive() {
        assert_eq!(quote("https://x"), "'https://x'");
        assert_eq!(quote("a'b"), r"'a'\''b'");
    }
}
