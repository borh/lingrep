//! Transparent benchmark-only launcher for recording `ling` and MeCab invocations.

use std::env;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{self, Command, ExitStatus};
use std::time::{SystemTime, UNIX_EPOCH};

const SCHEMA: &str = "lingrep-linguistic-eval.tool-invocation/v1";
const MAX_PATTERN_BYTES: u64 = 1024 * 1024;

struct Config {
    log_root: PathBuf,
    tool: String,
    real: PathBuf,
    arguments: Vec<OsString>,
}

struct PatternCapture {
    argument: OsString,
    resolved: PathBuf,
    capture: Option<String>,
    error: Option<String>,
}

fn unix_ns() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
}

fn json_string(value: &str) -> String {
    let mut encoded = String::from("\"");
    for character in value.chars() {
        match character {
            '"' => encoded.push_str("\\\""),
            '\\' => encoded.push_str("\\\\"),
            '\u{08}' => encoded.push_str("\\b"),
            '\u{0c}' => encoded.push_str("\\f"),
            '\n' => encoded.push_str("\\n"),
            '\r' => encoded.push_str("\\r"),
            '\t' => encoded.push_str("\\t"),
            value if value < '\u{20}' => {
                encoded.push_str(&format!("\\u{:04x}", value as u32));
            }
            value => encoded.push(value),
        }
    }
    encoded.push('"');
    encoded
}

fn os_json(value: &OsStr) -> String {
    json_string(&value.to_string_lossy())
}

fn parse_args(arguments: impl IntoIterator<Item = OsString>) -> Result<Config, String> {
    let mut arguments = arguments.into_iter();
    let _program = arguments.next();
    let mut log_root = None;
    let mut tool = None;
    let mut real = None;
    while let Some(argument) = arguments.next() {
        if argument == "--" {
            return Ok(Config {
                log_root: log_root.ok_or("missing --log-root")?,
                tool: tool.ok_or("missing --tool")?,
                real: real.ok_or("missing --real")?,
                arguments: arguments.collect(),
            });
        }
        let value = arguments
            .next()
            .ok_or_else(|| format!("missing value for {}", argument.to_string_lossy()))?;
        match argument.to_str() {
            Some("--log-root") => log_root = Some(PathBuf::from(value)),
            Some("--tool") => {
                tool = Some(
                    value
                        .into_string()
                        .map_err(|_| "--tool must be UTF-8".to_owned())?,
                );
            }
            Some("--real") => real = Some(PathBuf::from(value)),
            _ => {
                return Err(format!(
                    "unknown launcher argument: {}",
                    argument.to_string_lossy()
                ))
            }
        }
    }
    Err("missing -- separator".to_owned())
}

fn pattern_arguments(arguments: &[OsString]) -> Vec<OsString> {
    let mut patterns = Vec::new();
    let mut index = 0;
    while index < arguments.len() {
        // `-f` is the catalog flag's short spelling, so a launch that uses it is
        // as capturable as the long one. `--pattern` is the inline form and
        // names no file.
        if arguments[index] == "--patterns" || arguments[index] == "-f" {
            if let Some(value) = arguments.get(index + 1) {
                patterns.push(value.clone());
                index += 1;
            }
        } else if let Some(value) = arguments[index].to_str() {
            if let Some(path) = value.strip_prefix("--patterns=") {
                patterns.push(OsString::from(path));
            }
        }
        index += 1;
    }
    patterns
}

fn capture_patterns(directory: &Path, cwd: &Path, arguments: &[OsString]) -> Vec<PatternCapture> {
    pattern_arguments(arguments)
        .into_iter()
        .enumerate()
        .map(|(index, argument)| {
            let path = PathBuf::from(&argument);
            let resolved = if path.is_absolute() {
                path
            } else {
                cwd.join(path)
            };
            let capture = format!("patterns/{index}.lx");
            let destination = directory.join(&capture);
            let result = fs::metadata(&resolved).and_then(|metadata| {
                if metadata.len() > MAX_PATTERN_BYTES {
                    return Err(io::Error::new(
                        io::ErrorKind::FileTooLarge,
                        format!("pattern exceeds {MAX_PATTERN_BYTES} bytes"),
                    ));
                }
                fs::create_dir_all(destination.parent().expect("capture has parent"))?;
                fs::copy(&resolved, &destination)?;
                Ok(())
            });
            PatternCapture {
                argument,
                resolved,
                capture: result.as_ref().ok().map(|()| capture),
                error: result.err().map(|error| error.to_string()),
            }
        })
        .collect()
}

fn unique_directory(root: &Path) -> io::Result<(String, PathBuf)> {
    fs::create_dir_all(root)?;
    for attempt in 0..1000 {
        let identity = format!("{}-{}-{attempt}", unix_ns(), process::id());
        let directory = root.join(&identity);
        match fs::create_dir(&directory) {
            Ok(()) => return Ok((identity, directory)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not allocate invocation identity",
    ))
}

fn start_json(config: &Config, identity: &str, cwd: &Path, patterns: &[PatternCapture]) -> String {
    let argv = std::iter::once(OsStr::new(&config.tool))
        .chain(config.arguments.iter().map(OsString::as_os_str))
        .map(os_json)
        .collect::<Vec<_>>()
        .join(",");
    let patterns = patterns
        .iter()
        .map(|pattern| {
            format!(
                "{{\"argument\":{},\"resolved\":{},\"capture\":{},\"capture_error\":{}}}",
                os_json(&pattern.argument),
                os_json(pattern.resolved.as_os_str()),
                pattern
                    .capture
                    .as_deref()
                    .map(json_string)
                    .unwrap_or_else(|| "null".to_owned()),
                pattern
                    .error
                    .as_deref()
                    .map(json_string)
                    .unwrap_or_else(|| "null".to_owned()),
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "{{\"schema\":\"{SCHEMA}\",\"phase\":\"start\",\"invocation_id\":{},\"tool\":{},\"argv\":[{argv}],\"cwd\":{},\"real_executable\":{},\"launcher_pid\":{},\"parent_pid\":{},\"patterns\":[{patterns}]}}\n",
        json_string(identity),
        json_string(&config.tool),
        os_json(cwd.as_os_str()),
        os_json(config.real.as_os_str()),
        process::id(),
        std::os::unix::process::parent_id(),
    )
}

fn write_spawn(directory: &Path, identity: &str, started: u128, child_pid: u32) {
    let record = format!(
        "{{\"schema\":\"{SCHEMA}\",\"phase\":\"spawn\",\"invocation_id\":{},\"started_at_unix_ns\":{started},\"child_pid\":{child_pid}}}\n",
        json_string(identity),
    );
    let _ = fs::write(directory.join("spawn.json"), record);
}

fn write_finish(
    directory: &Path,
    identity: &str,
    status: Option<&ExitStatus>,
    spawn_error: Option<&io::Error>,
) {
    let exit_code = status
        .and_then(ExitStatus::code)
        .map(|value| value.to_string())
        .unwrap_or_else(|| "null".to_owned());
    let signal = status
        .and_then(ExitStatusExt::signal)
        .map(|value| value.to_string())
        .unwrap_or_else(|| "null".to_owned());
    let error = spawn_error
        .map(|value| json_string(&value.to_string()))
        .unwrap_or_else(|| "null".to_owned());
    let record = format!(
        "{{\"schema\":\"{SCHEMA}\",\"phase\":\"finish\",\"invocation_id\":{},\"finished_at_unix_ns\":{},\"exit_code\":{exit_code},\"signal\":{signal},\"spawn_error\":{error}}}\n",
        json_string(identity),
        unix_ns(),
    );
    let _ = fs::write(directory.join("finish.json"), record);
}

fn command(config: &Config) -> Command {
    let mut command = Command::new(&config.real);
    command.arg0(&config.tool).args(&config.arguments);
    command
}

fn exit_like(status: ExitStatus) -> ! {
    if let Some(code) = status.code() {
        process::exit(code);
    }
    let signal_number = status.signal().unwrap_or(1);
    // The child can terminate itself without signalling the launcher. Re-raise the
    // same signal so callers observe the real tool's termination semantics.
    unsafe {
        unsafe extern "C" {
            fn signal(number: i32, handler: usize) -> usize;
            fn raise(number: i32) -> i32;
        }
        const SIG_DFL: usize = 0;
        signal(signal_number, SIG_DFL);
        raise(signal_number);
    }
    process::exit(128 + signal_number);
}

fn run(config: Config) -> ! {
    let cwd = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let Ok((identity, directory)) = unique_directory(&config.log_root) else {
        let status = command(&config).status().unwrap_or_else(|error| {
            eprintln!(
                "{} launcher could not start real executable: {error}",
                config.tool
            );
            process::exit(127);
        });
        exit_like(status);
    };
    let patterns = if config.tool == "ling" {
        capture_patterns(&directory, &cwd, &config.arguments)
    } else {
        Vec::new()
    };
    if fs::write(
        directory.join("start.json"),
        start_json(&config, &identity, &cwd, &patterns),
    )
    .is_err()
    {
        let status = command(&config).status().unwrap_or_else(|error| {
            eprintln!(
                "{} launcher could not start real executable: {error}",
                config.tool
            );
            process::exit(127);
        });
        exit_like(status);
    }

    let started = unix_ns();
    match command(&config).spawn() {
        Ok(mut child) => {
            write_spawn(&directory, &identity, started, child.id());
            let status = child.wait().unwrap_or_else(|error| {
                eprintln!(
                    "{} launcher could not wait for real executable: {error}",
                    config.tool
                );
                process::exit(127);
            });
            write_finish(&directory, &identity, Some(&status), None);
            exit_like(status);
        }
        Err(error) => {
            write_finish(&directory, &identity, None, Some(&error));
            eprintln!(
                "{} launcher could not start real executable: {error}",
                config.tool
            );
            process::exit(127);
        }
    }
}

fn main() {
    let config = parse_args(env::args_os()).unwrap_or_else(|error| {
        eprintln!("tool launcher configuration error: {error}");
        process::exit(2);
    });
    run(config);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_every_catalog_spelling() {
        let arguments = vec![
            OsString::from("check"),
            OsString::from("--patterns"),
            OsString::from("first.lx"),
            OsString::from("--patterns=second.lx"),
            OsString::from("-f"),
            OsString::from("third.lx"),
        ];
        assert_eq!(
            pattern_arguments(&arguments),
            [
                OsString::from("first.lx"),
                OsString::from("second.lx"),
                OsString::from("third.lx")
            ]
        );
    }

    #[test]
    fn json_strings_preserve_unicode_and_escape_controls() {
        assert_eq!(json_string("日\n\"本"), "\"日\\n\\\"本\"");
    }
}
