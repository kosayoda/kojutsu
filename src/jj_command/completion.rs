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

pub fn complete(repo_path: &Path, input: &str) -> Vec<Completion> {
    let (completed, current) = split_for_completion(input);

    let mut args: Vec<&str> = vec!["--", "jj"];
    args.extend_from_slice(&completed);
    args.push(current);

    let Ok(output) = Command::new("jj")
        .args(&args)
        .env("COMPLETE", "fish")
        .current_dir(repo_path)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
    else {
        return Vec::new();
    };

    String::from_utf8_lossy(&output.stdout)
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

pub fn common_prefix(completions: &[Completion]) -> &str {
    if completions.is_empty() {
        return "";
    }
    let first = &completions[0].value;
    let mut len = first.len();
    for c in &completions[1..] {
        len = first
            .bytes()
            .zip(c.value.bytes())
            .take(len)
            .take_while(|(a, b)| a == b)
            .count();
    }
    &first[..len]
}
