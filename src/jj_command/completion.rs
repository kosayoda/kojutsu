use std::path::Path;
use std::process::Command;

pub struct Completion {
    pub value: String,
    pub description: String,
}

pub fn split_for_completion(input: &str) -> (Vec<&str>, &str) {
    if input.is_empty() {
        return (vec![], "");
    }
    if input.ends_with(char::is_whitespace) {
        let tokens: Vec<&str> = input.split_whitespace().collect();
        (tokens, "")
    } else {
        let parts: Vec<&str> = input.split_whitespace().collect();
        if parts.is_empty() {
            (vec![], "")
        } else {
            let (completed, current) = parts.split_at(parts.len() - 1);
            (completed.to_vec(), current[0])
        }
    }
}

/// How long jj gets to offer completions before it is given up on: past
/// this, the answer would come too late to be of use, and a jj stuck on a
/// lock or a slow filesystem would otherwise hold a process forever.
const COMPLETION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Ask jj's own shell completion what can follow `input`. Blocks while jj
/// works, so callers run it off the UI thread.
pub fn complete(repo_path: &Path, input: &str) -> Vec<Completion> {
    use std::io::Read as _;

    let (completed, current) = split_for_completion(input);

    let mut args: Vec<&str> = vec!["--", "jj"];
    args.extend_from_slice(&completed);
    args.push(current);

    let Ok(mut child) = Command::new("jj")
        .args(&args)
        .env("COMPLETE", "fish")
        .current_dir(repo_path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
    else {
        return Vec::new();
    };
    // Read while it runs: a long list would fill the pipe and stall jj
    // before it could exit.
    let Some(mut stdout) = child.stdout.take() else {
        return Vec::new();
    };
    let reader = std::thread::spawn(move || {
        let mut out = Vec::new();
        let _ = stdout.read_to_end(&mut out);
        out
    });
    let deadline = std::time::Instant::now() + COMPLETION_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Vec::new();
            }
        }
    }
    let stdout = reader.join().unwrap_or_default();

    String::from_utf8_lossy(&stdout)
        .lines()
        .map(|line| {
            let mut parts = line.splitn(2, '\t');
            let value = parts.next().unwrap_or("").to_string();
            let description = parts.next().unwrap_or("").to_string();
            Completion { value, description }
        })
        .filter(|c| !c.value.is_empty())
        .collect()
}

pub fn replace_current_token(input: &str, new_token: &str, add_space: bool) -> String {
    let (completed, _) = split_for_completion(input);
    let mut result = completed.join(" ");
    if !result.is_empty() {
        result.push(' ');
    }
    result.push_str(new_token);
    if add_space {
        result.push(' ');
    }
    result
}

/// The longest run of whole characters every completion starts with.
pub fn common_prefix(completions: &[Completion]) -> &str {
    let Some((first, rest)) = completions.split_first() else {
        return "";
    };
    rest.iter().fold(first.value.as_str(), |prefix, c| {
        let end = prefix
            .char_indices()
            .zip(c.value.chars())
            .find(|((_, a), b)| a != b)
            .map_or(prefix.len().min(c.value.len()), |((i, _), _)| i);
        &prefix[..end]
    })
}
