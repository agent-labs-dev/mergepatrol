//! Signing in through the browser (RFC 8252). The app listens on a loopback port and opens the
//! server's `/auth/login` with it, a `state`, and a PKCE challenge. After GitHub, the server
//! sends the browser to that port with a one-time code, which the app trades, with its verifier,
//! for a session token. The browser then goes on to the dashboard's `/tray` page, which says
//! how it went.

use std::io::{BufRead, BufReader, Write};
use std::net::{Ipv4Addr, TcpListener};
use std::time::{Duration, Instant};

const TIMEOUT: Duration = Duration::from_secs(5 * 60);
const POLL: Duration = Duration::from_millis(50);

/// Opens the browser and blocks until it comes back, with the token, or with why not.
pub fn run(server: &str) -> Result<String, String> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).map_err(|e| e.to_string())?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let server = server.trim_end_matches('/');
    let state = random_token(32)?;
    let verifier = random_token(64)?;
    let url = format!(
        "{server}/auth/login?tray_port={port}&tray_state={state}&tray_challenge={}",
        challenge(&verifier)
    );
    std::process::Command::new("/usr/bin/open")
        .arg(&url)
        .status()
        .map_err(|e| format!("could not open the browser: {e}"))?;

    // std's listener has no accept timeout, so it polls to notice the deadline.
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;
    let deadline = Instant::now() + TIMEOUT;
    loop {
        match listener.accept() {
            Ok((mut stream, _)) => {
                stream.set_nonblocking(false).map_err(|e| e.to_string())?;
                let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
                let mut line = String::new();
                let _ = BufReader::new(&stream).read_line(&mut line);
                // A favicon request, or anything else, is not the callback.
                let Some(outcome) = parse_callback(&line, &state) else {
                    let _ =
                        stream.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n");
                    continue;
                };
                let outcome =
                    outcome.and_then(|code| crate::api::trade_code(server, &code, &verifier));
                let page = match &outcome {
                    Ok(_) => format!("{server}/tray"),
                    Err(error) => format!("{server}/tray?error={}", percent_encode(error)),
                };
                let _ = write!(
                    stream,
                    "HTTP/1.1 302 Found\r\nLocation: {page}\r\nContent-Length: 0\r\n\
                     Connection: close\r\n\r\n"
                );
                return outcome;
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                if Instant::now() > deadline {
                    return Err("sign-in timed out".into());
                }
                std::thread::sleep(POLL);
            }
            Err(e) => return Err(e.to_string()),
        }
    }
}

/// `GET /callback?state=…&code=…` or `…&error=…`: the code, or why not. `None` for any other
/// request.
fn parse_callback(request_line: &str, expected_state: &str) -> Option<Result<String, String>> {
    let target = request_line.strip_prefix("GET ")?.split(' ').next()?;
    let query = target.strip_prefix("/callback?")?;
    let param = |name: &str| {
        query
            .split('&')
            .find_map(|p| p.strip_prefix(name)?.strip_prefix('='))
            .map(percent_decode)
    };
    if param("state").as_deref() != Some(expected_state) {
        return Some(Err(
            "the sign-in came back for another request: try again".into()
        ));
    }
    Some(match (param("code"), param("error")) {
        (Some(code), _) if !code.is_empty() => Ok(code),
        (_, Some(error)) => Err(error),
        _ => Err("the server sent no sign-in code".into()),
    })
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = bytes
            .get(i + 1..i + 3)
            .and_then(|h| std::str::from_utf8(h).ok())
            .and_then(|h| u8::from_str_radix(h, 16).ok());
        match (bytes[i], hex) {
            (b'%', Some(b)) => {
                out.push(b);
                i += 3;
            }
            (b'+', _) => {
                out.push(b' ');
                i += 1;
            }
            (b, _) => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn percent_encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// Alphanumeric, which both the `state` check and PKCE's verifier alphabet take.
fn random_token(len: usize) -> Result<String, String> {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    let mut bytes = vec![0u8; len];
    getrandom::fill(&mut bytes).map_err(|e| e.to_string())?;
    Ok(bytes
        .iter()
        .map(|b| ALPHABET[*b as usize % ALPHABET.len()] as char)
        .collect())
}

/// S256: base64url of the verifier's SHA-256, unpadded.
fn challenge(verifier: &str) -> String {
    use base64::Engine;
    use sha2::{Digest, Sha256};
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn takes_the_code_for_its_own_state() {
        assert_eq!(
            parse_callback("GET /callback?state=abc&code=c0de HTTP/1.1\r\n", "abc"),
            Some(Ok("c0de".into()))
        );
    }

    #[test]
    fn refuses_another_state() {
        assert!(matches!(
            parse_callback("GET /callback?state=evil&code=c0de HTTP/1.1", "abc"),
            Some(Err(_))
        ));
    }

    #[test]
    fn makes_the_rfc_7636_challenge() {
        // RFC 7636, appendix B.
        assert_eq!(
            challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn passes_the_servers_error_on() {
        assert_eq!(
            parse_callback(
                "GET /callback?state=abc&error=GitHub%20said%20no HTTP/1.1",
                "abc"
            ),
            Some(Err("GitHub said no".into()))
        );
    }

    #[test]
    fn ignores_other_requests() {
        assert_eq!(parse_callback("GET /favicon.ico HTTP/1.1", "abc"), None);
    }

    #[test]
    fn makes_a_state_the_server_takes() {
        let s = random_token(32).unwrap();
        assert_eq!(s.len(), 32);
        assert!(s.bytes().all(|b| b.is_ascii_alphanumeric()));
        assert_eq!(challenge(&s).len(), 43);
    }
}
