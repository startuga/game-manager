// PostToolUse hook (Edit|Write): formats a Rust file of this project right after Claude edits or
// writes it, with the project's toolchain (rust-toolchain.toml) and edition, as `cargo fmt` would.
// It never blocks: a file that doesn't parse yet is left as it is, and the build reports it.
const { execFileSync } = require("child_process");
const fs = require("fs");
const path = require("path");

let input = "";
process.stdin.on("data", (chunk) => (input += chunk));
process.stdin.on("end", () => {
  try {
    const event = JSON.parse(input);
    const file = event.tool_input && event.tool_input.file_path;
    const root = process.env.CLAUDE_PROJECT_DIR || event.cwd;
    if (!file || !root || !/\.rs$/i.test(file) || !fs.existsSync(file)) return;
    const inside = path.relative(root, file);
    if (inside.startsWith("..") || path.isAbsolute(inside) || /^target[\\/]/i.test(inside)) return;
    execFileSync("rustfmt", ["--edition", "2024", file], {
      cwd: root,
      stdio: "ignore",
      timeout: 20000,
    });
  } catch {
    // Formatting is a convenience: never get in the way.
  }
});
