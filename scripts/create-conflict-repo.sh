#!/bin/bash
set -e

DIR=/tmp/kj-conflict-test
rm -rf "$DIR"
mkdir -p "$DIR"
cd "$DIR"

jj git init

# Base content — multiple files with enough lines for multiple conflict regions
cat > main.rs << 'EOF'
use std::io;

fn greet(name: &str) -> String {
    format!("Hello, {}!", name)
}

fn add(a: i32, b: i32) -> i32 {
    a + b
}

fn main() {
    let greeting = greet("world");
    println!("{}", greeting);

    let result = add(2, 3);
    println!("2 + 3 = {}", result);
}
EOF

cat > config.toml << 'EOF'
[server]
host = "localhost"
port = 8080
timeout = 30

[database]
url = "postgres://localhost/mydb"
pool_size = 5
max_retries = 3

[logging]
level = "info"
format = "json"
EOF

cat > README.md << 'EOF'
# My Project

A simple demo project.

## Features

- Greeting function
- Addition function

## Usage

Run with `cargo run`.

## License

MIT
EOF

jj commit -m "base: initial files"
BASE=$(jj log -r @- --no-graph -T 'change_id.short()')

# Branch A
jj new "$BASE"
cat > main.rs << 'EOF'
use std::io;
use std::fmt;

fn greet(name: &str) -> String {
    format!("Hi there, {}! Welcome!", name)
}

fn add(a: i32, b: i32) -> i32 {
    a + b
}

fn multiply(a: i32, b: i32) -> i32 {
    a * b
}

fn main() {
    let greeting = greet("world");
    println!("{}", greeting);

    let sum = add(2, 3);
    println!("2 + 3 = {}", sum);

    let product = multiply(4, 5);
    println!("4 * 5 = {}", product);
}
EOF

cat > config.toml << 'EOF'
[server]
host = "0.0.0.0"
port = 9090
timeout = 60
workers = 4

[database]
url = "postgres://localhost/mydb"
pool_size = 10
max_retries = 5

[logging]
level = "debug"
format = "json"
output = "stdout"
EOF

cat > README.md << 'EOF'
# My Project

A simple demo project with math utilities.

## Features

- Greeting function
- Addition function
- Multiplication function

## Usage

Run with `cargo run`.

## Configuration

See `config.toml` for settings.

## License

MIT
EOF

jj commit -m "branch A: add multiply, expand config, update readme"
A=$(jj log -r @- --no-graph -T 'change_id.short()')

# Branch B
jj new "$BASE"
cat > main.rs << 'EOF'
use std::io;
use std::collections::HashMap;

fn greet(name: &str) -> String {
    format!("Good morning, {}!", name)
}

fn add(a: i32, b: i32) -> i32 {
    a + b
}

fn subtract(a: i32, b: i32) -> i32 {
    a - b
}

fn main() {
    let greeting = greet("world");
    println!("{}", greeting);

    let sum = add(10, 20);
    println!("10 + 20 = {}", sum);

    let diff = subtract(10, 3);
    println!("10 - 3 = {}", diff);
}
EOF

cat > config.toml << 'EOF'
[server]
host = "127.0.0.1"
port = 3000
timeout = 120
max_connections = 100

[database]
url = "postgres://localhost/mydb"
pool_size = 20
max_retries = 10

[logging]
level = "warn"
format = "text"
file = "/var/log/app.log"
EOF

cat > README.md << 'EOF'
# My Project

A robust demo project with arithmetic operations.

## Features

- Greeting function
- Addition function
- Subtraction function

## Usage

Run with `cargo run`.

## Deployment

See deployment docs for details.

## License

Apache-2.0
EOF

jj commit -m "branch B: add subtract, different config, update readme"
B=$(jj log -r @- --no-graph -T 'change_id.short()')

# Merge — should create conflicts in all three files
jj new "$A" "$B" -m "merge A and B"

echo ""
echo "=== Repo at $DIR ==="
jj log -r 'all()'
echo ""
echo "=== Status ==="
jj st
