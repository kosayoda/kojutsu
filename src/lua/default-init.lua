-- Kojutsu configuration.
-- Place this file at ~/.config/kojutsu/init.lua
--
-- Every value below is the built-in default, so this file is a no-op as
-- shipped: delete what you don't want to change. Assigning a section
-- replaces it, and any key you leave out falls back to its default — so
-- `kojutsu.config.theme = { accent = "red" }` changes only the accent.
--
-- Commands, hooks and keybindings live in this file too. Run
-- `kojutsu --generate-lua-types` for the full API, including
-- kojutsu.command, kojutsu.hook, kojutsu.bind and kojutsu.prefix.

-- Colors accept a name ("cyan", "dark_gray", "bright-white"), a hex string
-- ("#619eef"), an ANSI index (244), or a table ({ r = 97, g = 158, b = 239 }).
kojutsu.config.theme = {
  accent = "cyan", -- prompts, headers, visual range, immutable glyphs
  selection = "yellow", -- selected items, active toggles, keys
  muted = "dark_gray", -- borders, labels, disabled items, context lines
  text = "white", -- normal text, descriptions, file paths
  error = "red", -- errors, conflicts, removed lines
  warning = "yellow", -- committed without description, other warnings
  added = "green", -- added lines, working copy
  change_id = "magenta", -- change IDs, diff headers
  commit_id = "blue", -- commit IDs
  selection_bg = "#32323C", -- background highlight for the cursor row
  selection_bg_strong = "#3C3C4B", -- cursor row in the annotate view
  tag = "magenta", -- tag names in the tag view
  remote = "cyan", -- remote names (@git, @origin)
  bookmark = "magenta", -- bookmark names
  user = "yellow", -- author / user names
  workspace = "green", -- workspace names
}

-- Commit node glyphs in the DAG graph.
kojutsu.config.glyphs = {
  working_copy = "@",
  conflict = "×",
  immutable = "◆",
  merge = "⊕",
  normal = "○",
}

-- Search scopes enabled when starting a new search.
kojutsu.config.default_search_scopes = {
  change_id = true,
  commit_id = false,
  description = true,
  bookmark = false,
  author = false,
  path = false,
  line = false,
  tag = false,
}

-- Timestamp format (strftime syntax).
kojutsu.config.date_format = "%Y-%m-%d %H:%M:%S"

-- Tab width for diff rendering.
kojutsu.config.tab_width = 4

-- Max file size in MiB (per side) loaded into memory when computing a diff.
-- Larger files show a placeholder instead — a memory guard for repos with
-- huge text files. Applied at startup only.
kojutsu.config.diff = { max_file_size_mib = 64 }

-- Named revset presets; keys 1-5 switch between the first five.
-- kojutsu.config.revsets.presets = {
--   { name = "default", revset = "present(@) | ancestors(immutable_heads()..@, 2) | ancestors(trunk(), 16)" },
--   { name = "all mine", revset = "author(exact:'me@example.com')" },
--   { name = "recent",   revset = "heads(all())" },
--   { name = "conflicts", revset = "conflicted()" },
-- }

-- Commands offered by the `jj run` picker (the `!` prefix in the DAG view).
-- They run without a shell, in a private working copy per revision.
-- kojutsu.config.run.presets = { "cargo check", "cargo clippy" }

-- Note on large working copies: working-copy snapshots (startup, ctrl-r,
-- returning from suspended commands) scan the filesystem. To make them
-- incremental, enable watchman in your *jj* config (not this file):
--   [fsmonitor]
--   backend = "watchman"
