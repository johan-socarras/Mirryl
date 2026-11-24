use std::collections::BTreeMap;
use std::env;
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct AdbDevice {
    pub serial: String,
    pub state: String,
    pub properties: BTreeMap<String, String>,
}

impl AdbDevice {
    pub fn is_ready(&self) -> bool {
        self.state == "device"
    }

    pub fn model(&self) -> Option<&str> {
        self.properties.get("model").map(String::as_str)
    }

    pub fn product(&self) -> Option<&str> {
        self.properties.get("product").map(String::as_str)
    }
}

pub fn parse_devices(output: &str) -> Vec<AdbDevice> {
    output
        .lines()
        .map(str::trim)
        .filter(|line| {
            !line.is_empty()
                && !line.starts_with("List of devices attached")
                && !line.starts_with('*')
        })
        .filter_map(|line| {
            let mut parts = line.split_ascii_whitespace();
            let serial = parts.next()?.to_owned();
            let state = parts.next()?.to_owned();
            let mut properties = BTreeMap::new();

            for part in parts {
                if let Some((key, value)) = part.split_once(':')
                    && !key.is_empty()
                    && !value.is_empty()
                {
                    properties.insert(key.to_owned(), value.to_owned());
                }
            }

            Some(AdbDevice {
                serial,
                state,
                properties,
            })
        })
        .collect()
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct AdbEndpoint {
    pub host: String,
    pub port: u16,
}

impl AdbEndpoint {
    pub fn parse(value: &str) -> Result<Self, String> {
        let input = value.trim();
        if input.is_empty() {
            return Err("endpoint is required".to_owned());
        }

        let (host, port_text) = if let Some(after_open) = input.strip_prefix('[') {
            let close = after_open
                .find(']')
                .ok_or_else(|| "IPv6 endpoints must use [address]:port".to_owned())?;
            let host = &after_open[..close];
            let remainder = &after_open[(close + 1)..];
            let port = remainder
                .strip_prefix(':')
                .ok_or_else(|| "IPv6 endpoints must use [address]:port".to_owned())?;
            (host, port)
        } else {
            let (host, port) = input
                .rsplit_once(':')
                .ok_or_else(|| "endpoint must use host:port".to_owned())?;
            if host.contains(':') {
                return Err("IPv6 endpoints must use [address]:port".to_owned());
            }
            (host, port)
        };

        if host.is_empty() || host.chars().any(char::is_whitespace) {
            return Err("host is empty or contains whitespace".to_owned());
        }

        let port = port_text
            .parse::<u16>()
            .map_err(|_| "port must be between 1 and 65535".to_owned())?;
        if port == 0 {
            return Err("port must be between 1 and 65535".to_owned());
        }

        Ok(Self {
            host: host.to_owned(),
            port,
        })
    }
}

impl fmt::Display for AdbEndpoint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.host.contains(':') {
            write!(formatter, "[{}]:{}", self.host, self.port)
        } else {
            write!(formatter, "{}:{}", self.host, self.port)
        }
    }
}

#[derive(Debug)]
pub struct AdbResult {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl AdbResult {
    pub fn succeeded(&self) -> bool {
        self.exit_code == 0
    }

    pub fn combined_output(&self) -> String {
        [&self.stdout, &self.stderr]
            .into_iter()
            .map(|value| value.trim())
            .filter(|value| !value.is_empty())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

#[derive(Debug, Clone)]
pub struct AdbClient {
    executable: PathBuf,
}

impl AdbClient {
    /// A client for an adb the caller already located - the viewer keeps the
    /// path it was discovered with, and a reconnect needs a client again.
    pub fn from_executable(executable: PathBuf) -> Self {
        Self { executable }
    }

    pub fn discover() -> Result<Self, String> {
        if let Some(configured) = env::var_os("MIRADOR_ADB") {
            let path = PathBuf::from(configured);
            if !path.is_file() {
                return Err(format!(
                    "MIRADOR_ADB does not point to a file: {}",
                    path.display()
                ));
            }
            return Ok(Self { executable: path });
        }

        let names: &[&str] = if cfg!(windows) {
            &["adb.exe", "adb"]
        } else {
            &["adb"]
        };

        if let Ok(current_executable) = env::current_exe()
            && let Some(application_directory) = current_executable.parent()
        {
            for relative_directory in ["", "platform-tools"] {
                for name in names {
                    let candidate = application_directory.join(relative_directory).join(name);
                    if candidate.is_file() {
                        return Ok(Self {
                            executable: candidate,
                        });
                    }
                }
            }
        }

        for directory in env::split_paths(&env::var_os("PATH").unwrap_or_default()) {
            for name in names {
                let candidate = directory.join(name);
                if candidate.is_file() {
                    return Ok(Self {
                        executable: candidate,
                    });
                }
            }
        }

        Err(
            "ADB was not found. Install Android Platform Tools and add adb to \
             PATH, place platform-tools beside Mirador, or set MIRADOR_ADB."
                .to_owned(),
        )
    }

    pub fn executable(&self) -> &Path {
        &self.executable
    }

    pub fn run(&self, arguments: &[&str]) -> Result<AdbResult, String> {
        let mut command = Command::new(&self.executable);
        hide_process_window(&mut command);
        let output = command
            .args(arguments)
            .output()
            .map_err(|error| format!("failed to run ADB: {error}"))?;

        Ok(AdbResult {
            exit_code: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }

    pub fn devices(&self) -> Result<Vec<AdbDevice>, String> {
        let result = self.run(&["devices", "-l"])?;
        if !result.succeeded() {
            return Err(format!(
                "ADB could not enumerate devices: {}",
                result.combined_output()
            ));
        }
        Ok(parse_devices(&result.stdout))
    }

    pub fn pair(&self, endpoint: &AdbEndpoint, pairing_code: &str) -> Result<AdbResult, String> {
        if pairing_code.len() != 6 || !pairing_code.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err("Android pairing code must contain exactly six digits".to_owned());
        }
        self.run(&["pair", &endpoint.to_string(), pairing_code])
    }

    pub fn connect(&self, endpoint: &AdbEndpoint) -> Result<AdbResult, String> {
        self.run(&["connect", &endpoint.to_string()])
    }

    pub fn disconnect(&self, endpoint: &AdbEndpoint) -> Result<AdbResult, String> {
        self.run(&["disconnect", &endpoint.to_string()])
    }
}

fn hide_process_window(command: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;

        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    let _ = command;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ready_and_unauthorized_devices() {
        let sample = "\
List of devices attached
R58M1234567 device product:example model:Phone_X device:example transport_id:2
192.168.0.8:39841 unauthorized transport_id:3
";
        let devices = parse_devices(sample);
        assert_eq!(devices.len(), 2);
        assert_eq!(devices[0].serial, "R58M1234567");
        assert_eq!(devices[0].model(), Some("Phone_X"));
        assert!(devices[0].is_ready());
        assert!(!devices[1].is_ready());
    }

    #[test]
    fn parses_ipv4_endpoint() {
        let endpoint = AdbEndpoint::parse("192.168.0.8:37123").unwrap();
        assert_eq!(endpoint.host, "192.168.0.8");
        assert_eq!(endpoint.port, 37123);
        assert_eq!(endpoint.to_string(), "192.168.0.8:37123");
    }

    #[test]
    fn parses_bracketed_ipv6_endpoint() {
        let endpoint = AdbEndpoint::parse("[fe80::1234]:37123").unwrap();
        assert_eq!(endpoint.host, "fe80::1234");
        assert_eq!(endpoint.to_string(), "[fe80::1234]:37123");
    }

    #[test]
    fn rejects_unbracketed_ipv6_endpoint() {
        assert!(AdbEndpoint::parse("fe80::1234:37123").is_err());
    }
}
